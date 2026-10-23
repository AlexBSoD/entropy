# Display-menu characterization fixtures

`display-menu-catalog.json` contains 49 source-derived definition variants, not a closed list of keyboards accepted by Entropy. It retains matrix/layout/settings only (not unrelated keycode catalog metadata). QSID support in tests is synthesized from advertised fields; it is not a recording of physical capability replies.

Sources:
- 38 QMK definitions expanded read-only from the archived `macropad-delete-icons-20261019/firmware-source/keyboards/ergohaven` source snapshot. That archive has no Git provenance; do not attribute it to the user's flashed firmware.
- 11 RMK definitions from checkout `e9e0d5b5a0b47051e494ccb19ca7b795050f6a42`, including K04 standalone/mini/micro and all three Qube definitions.
- `source_sha256` hashes the expanded full source definition before removing unrelated fields. USB identity for definitions without IDs is derived from the corresponding keyboard revision; legacy Vial IDs remain separate.

Tests run USB for all variants and additionally synthetic Bluetooth policy for RMK variants, in English and Russian (120 model/transport/language rows). SM62 and unknown third-party capabilities are separate synthetic identity/protocol controls; no SM62 definition was present in that QMK archive. Macropad/SM62 Bluetooth support is not asserted.

Display-page eligibility means support for the existing Macropad/SM62 customization schema, not presence of a physical screen. K03 QMK OLED options, K03 RMK, K03Pro legacy LCD, Macropad v1, SM30, HPD LCD-ball variants, Planeta and receiver screens must not be conflated. Unknown physical display presence (including some HPD/Planeta/Phenom/Remnant variants) is not asserted absent. Generic Vial discovery and existing non-dedicated settings are unchanged.

K03Pro's LCD 318/319 declaration and legacy AA clock capability are retained. The pinned application contains no separate LCD brightness/timeout UI renderer for this schema: its dedicated Display renderer requires the Macropad RGB bundle or SM62. These tests do **not** claim to prove accessibility of a nonexistent legacy LCD UI control; adding such a route would be a separate feature beyond this repair. The actual OLED preset renderer is exercised.
