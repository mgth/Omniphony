/* Minimal link/ABI smoke test for liborender.
 *
 * Build & run (from the workspace root):
 *   cc -I orender_ffi/include orender_ffi/examples/smoke.c \
 *      -L target/debug -lorender -o /tmp/orender_smoke
 *   LD_LIBRARY_PATH=target/debug /tmp/orender_smoke
 *
 * Verifies the header compiles, the symbols link, version reporting works, and
 * the panic-safe boundary returns errors (not crashes) on bad input. It does
 * NOT exercise real decoding — that needs an encoded input file plus the
 * matching decoder bridge .so, and is the job of the full parity test.
 */
#include <stdio.h>
#include "orender.h"

int main(void) {
    printf("orender ABI %u.%u\n", orender_version_major(), orender_version_minor());

    /* Runtime handshake vs the header we compiled against. This test builds
     * library and header from the same tree, so both must match exactly (a
     * real consumer only requires major equality and probes symbols). */
    if (orender_version_major() != ORENDER_ABI_MAJOR) {
        printf("FAIL: runtime major %u != header %u\n",
               orender_version_major(), (unsigned)ORENDER_ABI_MAJOR);
        return 6;
    }
    if (orender_version_minor() != ORENDER_ABI_MINOR) {
        printf("FAIL: runtime minor %u != header %u\n",
               orender_version_minor(), (unsigned)ORENDER_ABI_MINOR);
        return 7;
    }

    /* Build id is static and never NULL. */
    const char *build_id = orender_build_id();
    if (build_id == NULL) { printf("FAIL: orender_build_id() == NULL\n"); return 8; }
    printf("build id: %s\n", build_id);

    /* set_option error contract: NULL handle/args -> -3 (never a crash). */
    if (orender_set_option(NULL, "nope", "x") != -3) {
        printf("FAIL: set_option(NULL handle) != -3\n");
        return 9;
    }

    /* The label enum is the byte contract of orender_channel_layout /
     * orender_bed_layout; spot-check the anchored values. */
    if (OrenderChannelLabel_L != 0 || OrenderChannelLabel_Lfe2 != 23 ||
        OrenderChannelLabel_Unknown != 255) {
        printf("FAIL: OrenderChannelLabel discriminants drifted\n");
        return 10;
    }

    /* An unresolvable bridge means creation must fail gracefully (NULL),
     * exercising the catch_unwind boundary rather than crashing.
     *
     * Pin BOTH paths to files that cannot exist so the run is hermetic on any
     * machine: a NULL config resolves to the user's shared config (whose OSC
     * settings would make this test race the machine's live renderer for the
     * OSC port), and even with no config the engine's last-resort search can
     * find a system-installed bridge — creation would then SUCCEED on a dev
     * box while failing in CI. Explicit dead paths defeat both fallbacks. */
    OrenderConfig cfg = {0};
    cfg.sample_rate = 48000;
    cfg.config_yaml_path = "/nonexistent/orender-smoke-config.yaml";
    cfg.bridge_path = "/nonexistent/orender-smoke-bridge.so";
    OrenderRenderer *r = orender_create(&cfg);
    if (r != NULL) {
        printf("FAIL: got a renderer without a bridge_path\n");
        orender_destroy(r);
        return 1;
    }
    printf("orender_create(no bridge) -> NULL (expected)\n");

    /* NULL-safety on the remaining entry points. */
    if (orender_channel_count(NULL) != 0) { printf("FAIL: channel_count(NULL)\n"); return 2; }
    if (orender_is_spatial(NULL) >= 0)    { printf("FAIL: is_spatial(NULL)\n"); return 3; }
    if (orender_channel_layout(NULL, NULL, 0) != 0) { printf("FAIL: channel_layout(NULL)\n"); return 5; }
    if (orender_drain(NULL, NULL, 0, NULL, NULL, NULL) >= 0) { printf("FAIL: drain(NULL)\n"); return 11; }
    if (orender_output_packet_pts(NULL, NULL) >= 0) { printf("FAIL: output_packet_pts(NULL)\n"); return 12; }
    orender_reset(NULL);
    orender_destroy(NULL);
    if (orender_create(NULL) != NULL)     { printf("FAIL: create(NULL)\n"); return 4; }

    printf("smoke OK\n");
    return 0;
}
