# Embedded typeface

`InterVariable-Latin.ttf` is a subset of Inter 4.1 by Rasmus Andersson and
the Inter Project Authors, from the official release
(<https://github.com/rsms/inter/releases/tag/v4.1>), under the SIL Open Font
License 1.1 in `OFL.txt`. Inter declares no Reserved Font Name, so the
subset keeps the name.

It holds every weight from 100 to 900 and the optical-size axis (14 to 32)
of the variable font, for 510 glyphs: Latin script for Western, Central and
Southern European languages, the punctuation and symbols of values and
units, arrows, and tabular figures. Hinting is removed, since the interface
renders at any size and scale.

Rebuild it, byte for byte, with `scripts/subset-font.py` and fontTools 4.63:

| File | SHA-256 |
|---|---|
| `Inter-4.1.zip` (input) | `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e` |
| `InterVariable-Latin.ttf` (output) | `7c1ec95d0ecfe8474afef66919512064750a5a2986cb975bcca712057be929f8` |
