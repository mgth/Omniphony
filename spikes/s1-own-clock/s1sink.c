/* S1 spike — the "orender sink" side, four clocking variants.
 *
 *   a0  pw_stream Audio/Sink, PW_STREAM_FLAG_DRIVER, triggered from an absolute
 *       CLOCK_MONOTONIC timerfd on the stream's data loop; spa_io_clock NOT filled
 *       (what orender does today, minus the main-loop/async placement).
 *   a   same, but the app fills spa_io_position.clock (nsec = ideal deadline,
 *       position, duration, rate, rate_diff = 1, next_nsec) before each trigger.
 *   c   pw_stream Audio/Sink as a plain FOLLOWER (no DRIVER flag) placed in a
 *       node.group together with an app-created `support.node.driver`
 *       (spa-node-factory, clock.id = monotonic) — PipeWire's own timer driver.
 *   b   app-created `support.null-audio-sink` adapter (node.driver = true) plus a
 *       follower capture stream on its monitor.
 *
 * Every test node is named rwspike-s1-*. No AUTOCONNECT on the sink, nothing is
 * linked to hardware. Throwaway code.
 */
#include "common.h"

#include <errno.h>
#include <getopt.h>
#include <signal.h>
#include <stdatomic.h>

#include <pipewire/pipewire.h>
#include <spa/node/io.h>
#include <spa/param/audio/format-utils.h>
#include <spa/param/audio/iec958.h>
#include <spa/param/latency-utils.h>
#include <spa/param/buffers.h>
#include <spa/pod/builder.h>
#include <spa/utils/result.h>

#define MAX_REC (1u << 20)

struct trec { /* timer wakeup (a0 / a) */
	uint64_t deadline, wake;
	uint64_t expirations;
};

struct prec { /* process callback */
	uint64_t t, nsec, next_nsec, position, duration;
	int64_t delay;
	double rate_diff;
	uint32_t rate, clock_id, bytes, nonzero, flags;
};

struct sink {
	struct pw_main_loop *main_loop;
	struct pw_context *context;
	struct pw_core *core;
	struct pw_stream *stream;
	struct spa_hook stream_listener;
	struct pw_proxy *aux; /* node-driver (c) or null sink (b) */
	struct spa_hook aux_listener;
	struct pw_loop *data_loop;
	struct spa_source *timer;

	char mode[8];
	char name[64];
	int duration_s;
	int64_t latency_ns;
	char latency_kind[16]; /* none | port | process */
	uint32_t want_rate;

	struct spa_io_position *_Atomic position;
	atomic_bool streaming;

	/* driver state (data loop only) */
	uint64_t anchor_ns, anchor_pos, pos;
	uint32_t cur_rate;
	uint64_t ndl; /* next deadline */

	struct trec *tr;
	size_t ntr;
	struct prec *pr;
	size_t npr;
	char fmt_desc[160];
	uint32_t format_changes;
	bool force_rate_follow;
	bool stall; /* robustness probe: block the process callback 30 ms every 250 cycles */
	bool no_rt; /* omit RT_PROCESS: node.loop.class=main, node.async=true (today's orender) */
	uint32_t forced_rate;
};

static uint64_t scale(uint64_t v, uint64_t num, uint64_t den)
{
	return (uint64_t)(((__uint128_t)v * num) / den);
}

/* ---------- driver timer (modes a0 / a), runs on the stream data loop ---------- */

static void arm(struct sink *s, uint64_t abs_ns)
{
	struct timespec ts = {.tv_sec = abs_ns / 1000000000ull, .tv_nsec = abs_ns % 1000000000ull};
	pw_loop_update_timer(s->data_loop, s->timer, &ts, NULL, true);
}

static void on_timer(void *data, uint64_t expirations)
{
	struct sink *s = data;
	uint64_t now = mono_ns();
	struct spa_io_position *p = atomic_load(&s->position);
	uint64_t q = 1024;
	uint32_t rate = s->want_rate;

	if (p && p->clock.target_rate.denom && p->clock.target_duration) {
		q = p->clock.target_duration;
		rate = p->clock.target_rate.denom;
	}
	if (rate != s->cur_rate) { /* re-anchor on a rate change: the deadline grid restarts */
		s->anchor_ns = s->ndl;
		s->anchor_pos = s->pos;
		s->cur_rate = rate;
	}
	uint64_t deadline = s->ndl;

	if (s->ntr < MAX_REC)
		s->tr[s->ntr++] = (struct trec){.deadline = deadline, .wake = now, .expirations = expirations};

	uint64_t next_pos = s->pos + q;
	uint64_t next_dl = s->anchor_ns + scale(next_pos - s->anchor_pos, 1000000000ull, rate);

	if (p && strcmp(s->mode, "a") == 0) {
		struct spa_io_clock *c = &p->clock;
		c->nsec = deadline;
		c->rate = SPA_FRACTION(1, rate);
		c->position = s->pos;
		c->duration = q;
		c->delay = 0;
		c->rate_diff = 1.0;
		c->next_nsec = next_dl;
		snprintf(c->name, sizeof(c->name), "rwspike.monotonic");
	}
	s->pos = next_pos;
	s->ndl = next_dl;
	arm(s, next_dl);

	if (atomic_load(&s->streaming))
		pw_stream_trigger_process(s->stream);
}

static int do_start_timer(struct spa_loop *loop, bool async, uint32_t seq, const void *data,
			  size_t size, void *user_data)
{
	struct sink *s = user_data;
	s->timer = pw_loop_add_timer(s->data_loop, on_timer, s);
	s->cur_rate = s->want_rate;
	s->anchor_ns = s->ndl = mono_ns() + 5000000ull;
	s->anchor_pos = s->pos = 0;
	arm(s, s->ndl);
	return 0;
}

static int do_stop_timer(struct spa_loop *loop, bool async, uint32_t seq, const void *data,
			 size_t size, void *user_data)
{
	struct sink *s = user_data;
	if (s->timer)
		pw_loop_destroy_source(s->data_loop, s->timer);
	s->timer = NULL;
	return 0;
}

/* ---------- stream callbacks ---------- */

static void on_io_changed(void *data, uint32_t id, void *area, uint32_t size)
{
	struct sink *s = data;
	if (id == SPA_IO_Position)
		atomic_store(&s->position, (struct spa_io_position *)area);
}

static void on_process(void *data)
{
	struct sink *s = data;
	uint64_t now = mono_ns();
	struct pw_buffer *b;
	uint32_t bytes = 0, nonzero = 0;

	while ((b = pw_stream_dequeue_buffer(s->stream)) != NULL) {
		struct spa_data *d = &b->buffer->datas[0];
		if (d->data && d->chunk) {
			const uint8_t *p = SPA_PTROFF(d->data, d->chunk->offset, const uint8_t);
			uint32_t sz = SPA_MIN(d->chunk->size, d->maxsize);
			bytes += sz;
			for (uint32_t i = 0; i < sz; i++)
				nonzero += p[i] != 0;
		}
		pw_stream_queue_buffer(s->stream, b);
	}
	struct spa_io_position *p = atomic_load(&s->position);
	if (s->stall && s->npr > 0 && s->npr % 250 == 0) {
		struct timespec st = {.tv_nsec = 30000000};
		nanosleep(&st, NULL);
	}
	if (s->npr < MAX_REC) {
		struct prec r = {.t = now, .bytes = bytes, .nonzero = nonzero};
		if (p) {
			r.nsec = p->clock.nsec;
			r.next_nsec = p->clock.next_nsec;
			r.position = p->clock.position;
			r.duration = p->clock.duration;
			r.delay = p->clock.delay;
			r.rate_diff = p->clock.rate_diff;
			r.rate = p->clock.rate.denom;
			r.clock_id = p->clock.id;
			r.flags = p->clock.flags;
		}
		s->pr[s->npr++] = r;
	}
}

static void on_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
	struct sink *s = data;
	if (id != SPA_PARAM_Format || param == NULL)
		return;
	uint32_t mt, mst;
	if (spa_format_parse(param, &mt, &mst) < 0)
		return;
	s->format_changes++;
	if (mst == SPA_MEDIA_SUBTYPE_iec958) {
		struct spa_audio_info_iec958 info = {0};
		spa_format_audio_iec958_parse(param, &info);
		snprintf(s->fmt_desc, sizeof(s->fmt_desc), "IEC958 codec=%u rate=%u", info.codec,
			 info.rate);
	} else if (mst == SPA_MEDIA_SUBTYPE_raw) {
		struct spa_audio_info_raw info = {0};
		spa_format_audio_raw_parse(param, &info);
		snprintf(s->fmt_desc, sizeof(s->fmt_desc), "raw format=%u rate=%u channels=%u",
			 info.format, info.rate, info.channels);
	} else {
		snprintf(s->fmt_desc, sizeof(s->fmt_desc), "subtype=%u", mst);
	}
	fprintf(stderr, "[sink] format negotiated: %s\n", s->fmt_desc);
	/* A 4x IEC958 carrier needs the graph at its rate; with clock.allowed-rates
	 * = [48000] the scheduler only moves there when a follower forces it. */
	uint32_t r = 0;
	if (mst == SPA_MEDIA_SUBTYPE_iec958) {
		struct spa_audio_info_iec958 info = {0};
		spa_format_audio_iec958_parse(param, &info);
		r = info.rate;
	}
	if (s->force_rate_follow && r != s->forced_rate) {
		char v[16];
		snprintf(v, sizeof(v), "%u", r);
		struct spa_dict_item it[] = {SPA_DICT_ITEM_INIT(PW_KEY_NODE_FORCE_RATE, v)};
		struct spa_dict d = SPA_DICT_INIT_ARRAY(it);
		int res = pw_stream_update_properties(s->stream, &d);
		fprintf(stderr, "[sink] node.force-rate=%s: %s\n", v, spa_strerror(res));
		s->forced_rate = r;
	}
}

static void on_state_changed(void *data, enum pw_stream_state old, enum pw_stream_state state,
			     const char *error)
{
	struct sink *s = data;
	fprintf(stderr, "[sink] stream state %s -> %s %s\n", pw_stream_state_as_string(old),
		pw_stream_state_as_string(state), error ? error : "");
	atomic_store(&s->streaming, state == PW_STREAM_STATE_STREAMING);
}

static const struct pw_stream_events stream_events = {
	PW_VERSION_STREAM_EVENTS,
	.state_changed = on_state_changed,
	.io_changed = on_io_changed,
	.param_changed = on_param_changed,
	.process = on_process,
};

/* ---------- params ---------- */

static const struct spa_pod *iec958_fmt(struct spa_pod_builder *b, uint32_t codec, uint32_t rate)
{
	struct spa_audio_info_iec958 info = {.codec = codec, .rate = rate};
	return spa_format_audio_iec958_build(b, SPA_PARAM_EnumFormat, &info);
}

static void publish_latency(struct sink *s)
{
	if (strcmp(s->latency_kind, "none") == 0)
		return;
	uint8_t buf[512];
	struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
	const struct spa_pod *param;
	if (strcmp(s->latency_kind, "process") == 0) {
		struct spa_process_latency_info pl = {.ns = s->latency_ns};
		param = spa_process_latency_build(&b, SPA_PARAM_ProcessLatency, &pl);
	} else {
		struct spa_latency_info li = SPA_LATENCY_INFO(SPA_DIRECTION_INPUT);
		li.min_ns = li.max_ns = s->latency_ns;
		param = spa_latency_build(&b, SPA_PARAM_Latency, &li);
	}
	int res = pw_stream_update_params(s->stream, &param, 1);
	fprintf(stderr, "[sink] published %s latency %" PRIi64 " ns: %s\n", s->latency_kind,
		s->latency_ns, spa_strerror(res));
}

/* ---------- main ---------- */

static void do_quit(void *data, int sig)
{
	pw_main_loop_quit(((struct sink *)data)->main_loop);
}

static void on_main_timeout(void *data, uint64_t exp)
{
	pw_main_loop_quit(((struct sink *)data)->main_loop);
}

static void report(struct sink *s)
{
	printf("=== s1sink mode=%s name=%s duration=%ds format=[%s] format_changes=%u\n", s->mode,
	       s->name, s->duration_s, s->fmt_desc, s->format_changes);
	/* skip the first 2 s of records (startup / negotiation) */
	size_t i0 = 0;
	if (s->ntr) {
		while (i0 < s->ntr && s->tr[i0].wake < s->tr[0].wake + 2000000000ull)
			i0++;
		size_t n = s->ntr - i0;
		double *v = malloc(n * sizeof(double));
		uint64_t multi = 0;
		for (size_t i = 0; i < n; i++) {
			v[i] = ((double)s->tr[i0 + i].wake - (double)s->tr[i0 + i].deadline) / 1000.0;
			multi += s->tr[i0 + i].expirations > 1;
		}
		stats_line("timer wake - deadline", v, n, "us");
		printf("  timer cycles=%zu with expirations>1: %" PRIu64 "\n", n, multi);
		free(v);
	}
	if (s->npr) {
		size_t j0 = 0;
		while (j0 < s->npr && s->pr[j0].t < s->pr[0].t + 2000000000ull)
			j0++;
		size_t n = s->npr - j0;
		double *v = malloc(n * sizeof(double)), *x = malloc(n * sizeof(double)),
		       *y = malloc(n * sizeof(double)), *dt = malloc(n * sizeof(double));
		uint64_t bytes = 0, nonzero = 0, zero_bytes_cycles = 0;
		double rd_min = INFINITY, rd_max = -INFINITY;
		int64_t dl_min = INT64_MAX, dl_max = INT64_MIN;
		uint32_t rate = 0;
		size_t ndt = 0;
		for (size_t i = 0; i < n; i++) {
			struct prec *r = &s->pr[j0 + i];
			v[i] = ((double)r->t - (double)r->nsec) / 1000.0;
			x[i] = (double)(r->position - s->pr[j0].position);
			y[i] = (double)r->nsec - (double)s->pr[j0].nsec;
			if (i > 0)
				dt[ndt++] = ((double)r->t - (double)s->pr[j0 + i - 1].t) / 1000.0;
			bytes += r->bytes;
			nonzero += r->nonzero;
			zero_bytes_cycles += r->bytes == 0;
			if (r->rate_diff < rd_min) rd_min = r->rate_diff;
			if (r->rate_diff > rd_max) rd_max = r->rate_diff;
			if (r->delay < dl_min) dl_min = r->delay;
			if (r->delay > dl_max) dl_max = r->delay;
			rate = r->rate;
		}
		stats_line("process t - clock.nsec", v, n, "us");
		stats_line("process period", dt, ndt, "us");
		double a, b, sd, pp;
		linfit(x, y, n, &a, &b, &sd, &pp);
		double nominal = rate ? 1e9 / rate : 0;
		printf("  clock.nsec vs clock.position fit: ns/frame=%.6f (nominal %.6f, %+.3f ppm) "
		       "resid std=%.3f us pp=%.3f us\n",
		       b, nominal, nominal ? (b / nominal - 1) * 1e6 : 0, sd / 1000, pp / 1000);
		printf("  clock: id=%u rate=1/%u duration=%" PRIu64 " rate_diff=[%.9f, %.9f] "
		       "delay=[%" PRIi64 ", %" PRIi64 "] first nsec=%" PRIu64 " last nsec=%" PRIu64 "\n",
		       s->pr[s->npr - 1].clock_id, rate, s->pr[s->npr - 1].duration, rd_min, rd_max,
		       dl_min, dl_max, s->pr[j0].nsec, s->pr[s->npr - 1].nsec);
		printf("  data: process cycles=%zu bytes=%" PRIu64 " nonzero_bytes=%" PRIu64
		       " cycles_without_data=%" PRIu64 "\n",
		       n, bytes, nonzero, zero_bytes_cycles);
		free(v);
		free(x);
		free(y);
		free(dt);
	}
	fflush(stdout);
}

int main(int argc, char *argv[])
{
	static struct sink s;
	snprintf(s.mode, sizeof(s.mode), "a");
	s.duration_s = 70;
	s.want_rate = 48000;
	snprintf(s.latency_kind, sizeof(s.latency_kind), "none");
	s.latency_ns = 0;
	const char *quantum = "1024";
	const char *csv = NULL;

	int c;
	while ((c = getopt(argc, argv, "m:d:l:k:r:q:o:FNS")) != -1) {
		switch (c) {
		case 'm': snprintf(s.mode, sizeof(s.mode), "%s", optarg); break;
		case 'd': s.duration_s = atoi(optarg); break;
		case 'l': s.latency_ns = (int64_t)(atof(optarg) * 1e6); break;
		case 'k': snprintf(s.latency_kind, sizeof(s.latency_kind), "%s", optarg); break;
		case 'r': s.want_rate = atoi(optarg); break;
		case 'q': quantum = optarg; break;
		case 'o': csv = optarg; break;
		case 'F': s.force_rate_follow = true; break;
		case 'N': s.no_rt = true; break;
		case 'S': s.stall = true; break;
		default:
			fprintf(stderr, "usage: s1sink -m a0|a|c|b [-d secs] [-k none|port|process -l ms] "
					"[-r rate] [-q quantum] [-o csv]\n");
			return 2;
		}
	}
	snprintf(s.name, sizeof(s.name), "rwspike-s1-%s", s.mode);
	s.tr = calloc(MAX_REC, sizeof(*s.tr));
	s.pr = calloc(MAX_REC, sizeof(*s.pr));

	pw_init(&argc, &argv);
	s.main_loop = pw_main_loop_new(NULL);
	struct pw_loop *ml = pw_main_loop_get_loop(s.main_loop);
	pw_loop_add_signal(ml, SIGINT, do_quit, &s);
	pw_loop_add_signal(ml, SIGTERM, do_quit, &s);
	s.context = pw_context_new(ml, NULL, 0);
	s.core = pw_context_connect(s.context, NULL, 0);
	if (!s.core) {
		fprintf(stderr, "connect failed\n");
		return 1;
	}

	char latency[32], rate_s[32], group[64];
	snprintf(latency, sizeof(latency), "%s/%u", quantum, s.want_rate);
	snprintf(rate_s, sizeof(rate_s), "1/%u", s.want_rate);
	snprintf(group, sizeof(group), "rwspike-s1-%s-group", s.mode);

	bool is_b = strcmp(s.mode, "b") == 0;
	bool is_c = strcmp(s.mode, "c") == 0;
	bool is_driver = !is_b && !is_c;

	if (is_c) {
		/* PipeWire's own timer driver, created by (and owned by) this client. */
		struct pw_properties *dp = pw_properties_new(
			"factory.name", "support.node.driver",
			"node.name", "rwspike-s1-c-clock",
			"node.description", "rwspike S1 monotonic clock",
			"node.group", group,
			"priority.driver", "0",
			"clock.id", "monotonic",
			"node.freewheel", "false",
			NULL);
		s.aux = pw_core_create_object(s.core, "spa-node-factory", PW_TYPE_INTERFACE_Node,
					      PW_VERSION_NODE, &dp->dict, 0);
		pw_properties_free(dp);
	}
	if (is_b) {
		struct pw_properties *np = pw_properties_new(
			"factory.name", "support.null-audio-sink",
			"node.name", "rwspike-s1-b",
			"node.description", "rwspike S1 null sink",
			"media.class", "Audio/Sink",
			"audio.position", "[ FL FR ]",
			"audio.rate", "48000",
			"node.driver", "true",
			"priority.driver", "0",
			"priority.session", "1",
			"monitor.channel-volumes", "false",
			"node.latency", latency,
			"object.linger", "false",
			NULL);
		s.aux = pw_core_create_object(s.core, "adapter", PW_TYPE_INTERFACE_Node,
					      PW_VERSION_NODE, &np->dict, 0);
		pw_properties_free(np);
	}

	struct pw_properties *props;
	if (is_b) {
		props = pw_properties_new(
			PW_KEY_MEDIA_TYPE, "Audio",
			PW_KEY_MEDIA_CATEGORY, "Capture",
			PW_KEY_NODE_NAME, "rwspike-s1-b-capture",
			PW_KEY_TARGET_OBJECT, "rwspike-s1-b",
			PW_KEY_STREAM_CAPTURE_SINK, "true",
			"node.dont-fallback", "true",
			PW_KEY_NODE_DONT_RECONNECT, "true",
			PW_KEY_NODE_LATENCY, latency,
			NULL);
	} else {
		props = pw_properties_new(
			PW_KEY_MEDIA_TYPE, "Audio",
			PW_KEY_MEDIA_CATEGORY, "Playback",
			PW_KEY_MEDIA_ROLE, "Movie",
			PW_KEY_MEDIA_CLASS, "Audio/Sink",
			PW_KEY_NODE_VIRTUAL, "true",
			PW_KEY_NODE_NAME, s.name,
			PW_KEY_NODE_DESCRIPTION, "rwspike S1 test sink",
			"priority.session", "1",
			"audio.channels", "2",
			"audio.position", "[ FL FR ]",
			"iec958.codecs", "[ \"AC3\", \"EAC3\", \"TRUEHD\", \"DTS\", \"DTSHD\" ]",
			"resample.disable", "true",
			PW_KEY_NODE_LATENCY, latency,
			PW_KEY_NODE_RATE, rate_s,
			PW_KEY_NODE_ALWAYS_PROCESS, "true",
			NULL);
		if (is_c)
			pw_properties_set(props, PW_KEY_NODE_GROUP, group);
	}

	s.stream = pw_stream_new(s.core, s.name, props);
	pw_stream_add_listener(s.stream, &s.stream_listener, &stream_events, &s);

	uint8_t buf[4096];
	struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
	const struct spa_pod *params[10];
	uint32_t n = 0;
	if (!is_b) {
		params[n++] = iec958_fmt(&b, SPA_AUDIO_IEC958_CODEC_AC3, 48000);
		params[n++] = iec958_fmt(&b, SPA_AUDIO_IEC958_CODEC_EAC3, 192000);
		params[n++] = iec958_fmt(&b, SPA_AUDIO_IEC958_CODEC_TRUEHD, 192000);
		params[n++] = iec958_fmt(&b, SPA_AUDIO_IEC958_CODEC_DTS, 48000);
		params[n++] = iec958_fmt(&b, SPA_AUDIO_IEC958_CODEC_DTSHD, 192000);
	}
	struct spa_audio_info_raw raw = {.format = SPA_AUDIO_FORMAT_F32, .rate = s.want_rate,
					 .channels = 2, .position = {SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR}};
	params[n++] = spa_format_audio_raw_build(&b, SPA_PARAM_EnumFormat, &raw);
	/* Buffers big enough for one quantum-limit cycle at the widest stride (8ch x
	 * 16-bit IEC958 / 2ch f32): otherwise a 4x carrier gets half a quantum per cycle. */
	if (!is_b)
		params[n++] = spa_pod_builder_add_object(&b,
			SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
			SPA_PARAM_BUFFERS_buffers, SPA_POD_CHOICE_RANGE_Int(4, 2, 16),
			SPA_PARAM_BUFFERS_blocks, SPA_POD_Int(1),
			SPA_PARAM_BUFFERS_size, SPA_POD_CHOICE_RANGE_Int(8192 * 16, 4096, 8192 * 16),
			SPA_PARAM_BUFFERS_stride, SPA_POD_CHOICE_RANGE_Int(4, 1, 16));

	enum pw_stream_flags flags = PW_STREAM_FLAG_MAP_BUFFERS;
	if (!s.no_rt)
		flags |= PW_STREAM_FLAG_RT_PROCESS;
	if (is_driver)
		flags |= PW_STREAM_FLAG_DRIVER;
	if (is_b)
		flags |= PW_STREAM_FLAG_AUTOCONNECT;

	int res = pw_stream_connect(s.stream, SPA_DIRECTION_INPUT, PW_ID_ANY, flags, params, n);
	if (res < 0) {
		fprintf(stderr, "connect: %s\n", spa_strerror(res));
		return 1;
	}
	s.data_loop = pw_stream_get_data_loop(s.stream);
	if (!is_b)
		publish_latency(&s);
	if (is_driver)
		pw_loop_invoke(s.data_loop, do_start_timer, 0, NULL, 0, true, &s);

	struct spa_source *quit_timer = pw_loop_add_timer(ml, on_main_timeout, &s);
	struct timespec to = {.tv_sec = s.duration_s};
	pw_loop_update_timer(ml, quit_timer, &to, NULL, false);

	fprintf(stderr, "[sink] mode=%s node=%s running %ds\n", s.mode, s.name, s.duration_s);
	pw_main_loop_run(s.main_loop);

	if (is_driver)
		pw_loop_invoke(s.data_loop, do_stop_timer, 0, NULL, 0, true, &s);
	pw_stream_disconnect(s.stream);
	report(&s);

	if (csv) {
		FILE *f = fopen(csv, "w");
		if (f) {
			fprintf(f, "kind,t,nsec_or_deadline,position,duration,rate,rate_diff,delay,bytes\n");
			for (size_t i = 0; i < s.ntr; i++)
				fprintf(f, "T,%" PRIu64 ",%" PRIu64 ",,,,,,\n", s.tr[i].wake, s.tr[i].deadline);
			for (size_t i = 0; i < s.npr; i++)
				fprintf(f, "P,%" PRIu64 ",%" PRIu64 ",%" PRIu64 ",%" PRIu64 ",%u,%.9f,%" PRIi64 ",%u\n",
					s.pr[i].t, s.pr[i].nsec, s.pr[i].position, s.pr[i].duration,
					s.pr[i].rate, s.pr[i].rate_diff, s.pr[i].delay, s.pr[i].bytes);
			fclose(f);
		}
	}

	pw_stream_destroy(s.stream);
	if (s.aux)
		pw_proxy_destroy(s.aux);
	pw_core_disconnect(s.core);
	pw_context_destroy(s.context);
	pw_main_loop_destroy(s.main_loop);
	pw_deinit();
	return 0;
}
