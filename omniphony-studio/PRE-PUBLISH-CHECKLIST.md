# Omniphony Studio Pre-Publish Checklist

Use this checklist before any release, tagged build, or distributed package.

## IA Checks

These checks are suitable for automation, scripting, static analysis, or AI-assisted review.

- Verify the application starts cleanly in development mode.
- Verify the production build completes successfully.
- Verify no unexpected console errors or warnings appear during normal use.

- Verify OSC connection, reconnection, and error states behave as expected.
- Verify layout loading, layout switching, import, and export flows.
- Verify speaker editing, object selection, and live updates in the scene.
- Verify telemetry, meters, and status indicators update correctly.
- Verify audio output controls, latency controls, and renderer controls apply correctly.
- Verify no panel expansion or collapse causes viewport jumps or incorrect canvas resizing.

- Verify all supported languages are present in the language selector.
- Verify each supported locale file exists and is loadable.
- Verify every translation key used by the UI exists in every supported language.
- Verify no translation falls back unexpectedly to English unless that fallback is intentional.
- Verify recently added UI strings are present in all supported languages.
- Verify live language switching updates both static labels and dynamic UI text.
- Verify titles, tooltips, empty states, modal copy, and status messages are translated.

- Verify version, metadata, and release notes are up to date.
- Verify assets required for distribution are present and current.
- Verify no temporary debugging code, logs, or test hooks remain enabled.
- Verify the git diff only contains intended release changes.
- Verify documentation affected by the release has been updated.

## Human Checks

These checks require human judgment, visual validation, or release sign-off.

- Verify the packaged application starts without startup errors.
- Verify the main 3D scene renders correctly on first launch.
- Verify UI panels, overlays, and modals open and close correctly.
- Verify translation quality, wording consistency, and formatting for each supported language.

## Final Sign-Off

- Record who performed the checks.
- Record the date of verification.
- Record the exact commit or tag that was validated.
- Record any known limitations accepted for publication.
