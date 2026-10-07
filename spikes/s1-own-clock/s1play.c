/* S1 spike — a minimal "player" follower (stands in for mpv's ao_pipewire).
 * Plays digital silence into target.object (raw S16 2ch, or IEC958 with a codec)
 * and records, every process cycle, what a follower sees: pw_stream_get_time_n()
 * and the raw spa_io_position clock. It also reproduces mpv's end_time formula
 * (ao_pipewire.c on_process) to show the clock mpv would derive.
 * node.dont-fallback / dont-reconnect: never lands on the default sink.
 */
#include "common.h"

#include <getopt.h>
#include <signal.h>
#include <stdatomic.h>

#include <pipewire/pipewire.h>
#include <spa/node/io.h>
#include <spa/param/audio/format-utils.h>
#include <spa/param/audio/iec958.h>
#include <spa/pod/builder.h>
#include <spa/utils/result.h>

#define MAX_REC (1u << 20)

struct rec {
	uint64_t t1, t2; /* mp_time_ns() and pw_stream_get_nsec() stand-ins */
	struct pw_time tm;
	uint64_t clk_nsec, clk_next, clk_pos, clk_dur;
	double clk_rd;
	uint32_t clk_id, nframes, requested;
};

struct play {
	struct pw_main_loop *main_loop;
	struct pw_context *context;
	struct pw_core *core;
	struct pw_stream *stream;
	struct spa_hook listener;
	struct spa_io_position *_Atomic position;
	uint32_t rate, stride;
	char codec[16];
	char target[64];
	int duration_s;
	struct rec *r;
	size_t n;
	char clock_name[64];
	char fmt_desc[128];
};

static void on_io_changed(void *data, uint32_t id, void *area, uint32_t size)
{
	struct play *p = data;
	if (id == SPA_IO_Position)
		atomic_store(&p->position, (struct spa_io_position *)area);
}

static void on_process(void *data)
{
	struct play *p = data;
	struct pw_buffer *b = pw_stream_dequeue_buffer(p->stream);
	if (!b)
		return;
	struct spa_buffer *buf = b->buffer;
	uint32_t nframes = buf->datas[0].maxsize / p->stride;
	if (b->requested)
		nframes = SPA_MIN((uint32_t)b->requested, nframes);

	struct rec r = {0};
	pw_stream_get_time_n(p->stream, &r.tm, sizeof(r.tm));
	r.t1 = mono_ns();
	r.t2 = mono_ns();
	r.nframes = nframes;
	r.requested = b->requested;
	struct spa_io_position *pos = atomic_load(&p->position);
	if (pos) {
		r.clk_nsec = pos->clock.nsec;
		r.clk_next = pos->clock.next_nsec;
		r.clk_pos = pos->clock.position;
		r.clk_dur = pos->clock.duration;
		r.clk_rd = pos->clock.rate_diff;
		r.clk_id = pos->clock.id;
		if (!p->clock_name[0])
			memcpy(p->clock_name, pos->clock.name, sizeof(p->clock_name) - 1);
	}
	if (p->n < MAX_REC)
		p->r[p->n++] = r;

	memset(buf->datas[0].data, 0, nframes * p->stride); /* digital silence */
	buf->datas[0].chunk->offset = 0;
	buf->datas[0].chunk->stride = p->stride;
	buf->datas[0].chunk->size = nframes * p->stride;
	pw_stream_queue_buffer(p->stream, b);
}

static void on_state_changed(void *data, enum pw_stream_state old, enum pw_stream_state state,
			     const char *error)
{
	fprintf(stderr, "[play] state %s -> %s %s\n", pw_stream_state_as_string(old),
		pw_stream_state_as_string(state), error ? error : "");
	if (state == PW_STREAM_STATE_ERROR)
		pw_main_loop_quit(((struct play *)data)->main_loop);
}

static void on_param_changed(void *data, uint32_t id, const struct spa_pod *param)
{
	struct play *p = data;
	if (id != SPA_PARAM_Format || !param)
		return;
	uint32_t mt, mst;
	spa_format_parse(param, &mt, &mst);
	snprintf(p->fmt_desc, sizeof(p->fmt_desc), "%s",
		 mst == SPA_MEDIA_SUBTYPE_iec958 ? "IEC958" : "raw");
	fprintf(stderr, "[play] format negotiated: %s\n", p->fmt_desc);
}

static const struct pw_stream_events events = {
	PW_VERSION_STREAM_EVENTS,
	.state_changed = on_state_changed,
	.io_changed = on_io_changed,
	.param_changed = on_param_changed,
	.process = on_process,
};

static void do_quit(void *data, int sig) { pw_main_loop_quit(((struct play *)data)->main_loop); }
static void on_timeout(void *data, uint64_t e) { pw_main_loop_quit(((struct play *)data)->main_loop); }

static void report(struct play *p)
{
	printf("=== s1play target=%s codec=%s rate=%u format=[%s] clock.name=\"%s\"\n", p->target,
	       p->codec, p->rate, p->fmt_desc, p->clock_name);
	if (p->n < 10) {
		printf("  only %zu cycles recorded\n", p->n);
		return;
	}
	size_t i0 = 0;
	while (i0 < p->n && p->r[i0].t1 < p->r[0].t1 + 2000000000ull)
		i0++;
	size_t n = p->n - i0;
	double *wake = malloc(n * 8), *x = malloc(n * 8), *y = malloc(n * 8), *e = malloc(n * 8),
	       *lat = malloc(n * 8), *dly = malloc(n * 8);
	double rd_min = INFINITY, rd_max = -INFINITY;
	int64_t d_min = INT64_MAX, d_max = INT64_MIN, q_min = INT64_MAX, q_max = INT64_MIN,
		bf_min = INT64_MAX, bf_max = INT64_MIN;
	uint64_t cum = 0;
	uint32_t rnum = 1, rden = p->rate;
	uint64_t stale = 0;
	for (size_t i = 0; i < n; i++) {
		struct rec *r = &p->r[i0 + i];
		if (r->tm.rate.denom) {
			rnum = r->tm.rate.num;
			rden = r->tm.rate.denom;
		}
		double ns_per_tick = 1e9 * rnum / rden;
		wake[i] = ((double)r->t1 - (double)r->tm.now) / 1000.0;
		x[i] = (double)(r->tm.ticks - p->r[i0].tm.ticks);
		y[i] = (double)r->tm.now - (double)p->r[i0].tm.now;
		/* mpv ao_pipewire end_time */
		double end = (double)r->t1 + r->nframes * 1e9 / p->rate + r->tm.delay * ns_per_tick +
			     r->tm.queued * 1e9 / p->rate + r->tm.buffered * 1e9 / p->rate -
			     ((double)r->t2 - (double)r->tm.now);
		/* frames handed out before this callback; end_time should equal
		 * end_0 + (cum + nframes)/rate on a clean clock */
		e[i] = end - (double)p->r[i0].t1 - (cum + r->nframes) * 1e9 / p->rate;
		lat[i] = (end - (double)r->t1) / 1e6; /* ms: latency mpv believes at write time */
		dly[i] = r->tm.delay * ns_per_tick / 1e6;
		cum += r->nframes;
		if (i && r->tm.now == p->r[i0 + i - 1].tm.now)
			stale++;
		if (r->clk_rd < rd_min) rd_min = r->clk_rd;
		if (r->clk_rd > rd_max) rd_max = r->clk_rd;
		if (r->tm.delay < d_min) d_min = r->tm.delay;
		if (r->tm.delay > d_max) d_max = r->tm.delay;
		int64_t qq = (int64_t)r->tm.queued, bb = (int64_t)r->tm.buffered;
		if (qq < q_min) q_min = qq;
		if (qq > q_max) q_max = qq;
		if (bb < bf_min) bf_min = bb;
		if (bb > bf_max) bf_max = bb;
	}
	double e0 = e[0];
	for (size_t i = 0; i < n; i++)
		e[i] = (e[i] - e0) / 1000.0;
	struct rec *last = &p->r[p->n - 1];
	printf("  cycles=%zu (after 2 s warm-up) clock.id=%u duration=%" PRIu64 " rate=%u/%u\n", n,
	       last->clk_id, last->clk_dur, rnum, rden);
	printf("  pw_time.now repeated (stale) on %" PRIu64 " cycles; first now=%" PRIu64
	       " last now=%" PRIu64 " last wake=%" PRIu64 "\n",
	       stale, p->r[i0].tm.now, last->tm.now, last->t1);
	stats_line("wake - pw_time.now", wake, n, "us");
	double a, b, sd, pp;
	linfit(x, y, n, &a, &b, &sd, &pp);
	double nominal = 1e9 * rnum / rden;
	printf("  pw_time.now vs ticks fit: ns/tick=%.6f (nominal %.6f, %+.3f ppm) resid std=%.3f us "
	       "pp=%.3f us\n",
	       b, nominal, nominal ? (b / nominal - 1) * 1e6 : 0, sd / 1000, pp / 1000);
	printf("  rate_diff=[%.9f, %.9f] pw_time.delay=[%" PRIi64 ", %" PRIi64 "] ticks queued=[%" PRIi64
	       ", %" PRIi64 "] buffered=[%" PRIi64 ", %" PRIi64 "]\n",
	       rd_min, rd_max, d_min, d_max, q_min, q_max, bf_min, bf_max);
	stats_line("pw_time.delay", dly, n, "ms");
	stats_line("mpv end_time drift vs frames", e, n, "us");
	stats_line("mpv-believed latency at write", lat, n, "ms");
	fflush(stdout);
}

int main(int argc, char *argv[])
{
	static struct play p;
	p.rate = 48000;
	p.duration_s = 65;
	snprintf(p.codec, sizeof(p.codec), "pcm");
	snprintf(p.target, sizeof(p.target), "rwspike-s1-a");
	const char *latency = "1024/48000";
	int c;
	while ((c = getopt(argc, argv, "t:c:d:r:l:")) != -1) {
		switch (c) {
		case 't': snprintf(p.target, sizeof(p.target), "%s", optarg); break;
		case 'c': snprintf(p.codec, sizeof(p.codec), "%s", optarg); break;
		case 'd': p.duration_s = atoi(optarg); break;
		case 'r': p.rate = atoi(optarg); break;
		case 'l': latency = optarg; break;
		default:
			fprintf(stderr, "usage: s1play -t target [-c pcm|ac3|eac3|truehd] [-d s] [-r rate]\n");
			return 2;
		}
	}
	if (strncmp(p.target, "rwspike-s1-", 11) != 0) {
		fprintf(stderr, "refusing a target outside rwspike-s1-*\n");
		return 2;
	}
	p.r = calloc(MAX_REC, sizeof(*p.r));
	pw_init(&argc, &argv);
	p.main_loop = pw_main_loop_new(NULL);
	struct pw_loop *ml = pw_main_loop_get_loop(p.main_loop);
	pw_loop_add_signal(ml, SIGINT, do_quit, &p);
	pw_loop_add_signal(ml, SIGTERM, do_quit, &p);
	p.context = pw_context_new(ml, NULL, 0);
	p.core = pw_context_connect(p.context, NULL, 0);
	if (!p.core)
		return 1;

	struct pw_properties *props = pw_properties_new(
		PW_KEY_MEDIA_TYPE, "Audio",
		PW_KEY_MEDIA_CATEGORY, "Playback",
		PW_KEY_MEDIA_ROLE, "Movie",
		PW_KEY_NODE_NAME, "rwspike-s1-player",
		PW_KEY_TARGET_OBJECT, p.target,
		"node.dont-fallback", "true",
		PW_KEY_NODE_DONT_RECONNECT, "true",
		PW_KEY_NODE_LATENCY, latency,
		NULL);
	p.stream = pw_stream_new(p.core, "rwspike-s1-player", props);
	pw_stream_add_listener(p.stream, &p.listener, &events, &p);

	uint8_t buf[1024];
	struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
	const struct spa_pod *params[1];
	if (strcmp(p.codec, "pcm") == 0) {
		struct spa_audio_info_raw raw = {.format = SPA_AUDIO_FORMAT_S16, .rate = p.rate,
						 .channels = 2,
						 .position = {SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR}};
		params[0] = spa_format_audio_raw_build(&b, SPA_PARAM_EnumFormat, &raw);
		p.stride = 4;
	} else {
		struct spa_audio_info_iec958 info = {.rate = p.rate};
		if (strcmp(p.codec, "ac3") == 0) info.codec = SPA_AUDIO_IEC958_CODEC_AC3;
		else if (strcmp(p.codec, "eac3") == 0) info.codec = SPA_AUDIO_IEC958_CODEC_EAC3;
		else if (strcmp(p.codec, "truehd") == 0) info.codec = SPA_AUDIO_IEC958_CODEC_TRUEHD;
		params[0] = spa_format_audio_iec958_build(&b, SPA_PARAM_EnumFormat, &info);
		p.stride = strcmp(p.codec, "truehd") == 0 ? 16 : 4;
	}
	int res = pw_stream_connect(p.stream, SPA_DIRECTION_OUTPUT, PW_ID_ANY,
				    PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS |
					    PW_STREAM_FLAG_RT_PROCESS,
				    params, 1);
	if (res < 0) {
		fprintf(stderr, "connect: %s\n", spa_strerror(res));
		return 1;
	}
	struct spa_source *t = pw_loop_add_timer(ml, on_timeout, &p);
	struct timespec to = {.tv_sec = p.duration_s};
	pw_loop_update_timer(ml, t, &to, NULL, false);
	pw_main_loop_run(p.main_loop);
	pw_stream_disconnect(p.stream);
	report(&p);
	pw_stream_destroy(p.stream);
	pw_core_disconnect(p.core);
	pw_context_destroy(p.context);
	pw_main_loop_destroy(p.main_loop);
	pw_deinit();
	return 0;
}
