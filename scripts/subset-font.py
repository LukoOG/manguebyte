"""Builds the typeface the user interface embeds, from the official Inter release.

    python scripts/subset-font.py path/to/Inter-4.1.zip

Download Inter-4.1.zip from https://github.com/rsms/inter/releases/tag/v4.1
first, and install fontTools (pip install fonttools). The script checks the
archive against the SHA-256 recorded below, takes the variable font and its
licence out of it, keeps the characters and features listed below, and writes
crates/manguebyte-gfx/fonts/InterVariable-Latin.ttf and OFL.txt.

Inter is under the SIL Open Font License 1.1 and declares no Reserved Font
Name, so the subset may keep the name; the licence travels with it.
"""

import hashlib
import io
import pathlib
import sys
import zipfile

from fontTools import subset
from fontTools.ttLib import TTFont

RELEASE_SHA256 = "9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e"
FONT_IN_ARCHIVE = "InterVariable.ttf"
LICENCE_IN_ARCHIVE = "LICENSE.txt"

# What an audio interface writes: Latin script for Western, Central and
# Southern European languages, the punctuation and symbols of values and
# units, and arrows.
UNICODES = [
    (0x0020, 0x007E),  # Basic Latin
    (0x00A0, 0x00FF),  # Latin-1 Supplement: accents, degree, micro, plus-minus
    (0x0100, 0x017F),  # Latin Extended-A
    (0x2010, 0x2027),  # dashes, quotation marks, bullet, ellipsis
    (0x2030, 0x2030),  # per mille
    (0x2032, 0x2033),  # prime, double prime
    (0x2039, 0x203A),  # single angle quotation marks
    (0x20AC, 0x20AC),  # euro
    (0x2122, 0x2122),  # trade mark
    (0x2190, 0x2195),  # arrows
    (0x2212, 0x2212),  # minus sign
    (0x221E, 0x221E),  # infinity
    (0x2248, 0x2248),  # almost equal
    (0x2260, 0x2260),  # not equal
    (0x2264, 0x2265),  # less and greater than or equal
]

# Kerning and the usual ligatures, plus tabular figures, so that a changing
# value does not make its neighbours move.
FEATURES = ["kern", "liga", "calt", "ccmp", "locl", "mark", "mkmk", "tnum", "case"]


def main(archive_path: str) -> None:
    data = pathlib.Path(archive_path).read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != RELEASE_SHA256:
        sys.exit(f"{archive_path} is not the Inter 4.1 release (SHA-256 {digest})")
    archive = zipfile.ZipFile(io.BytesIO(data))

    # Keep the release's own timestamp, so that the output is the same on every run.
    font = TTFont(io.BytesIO(archive.read(FONT_IN_ARCHIVE)), recalcTimestamp=False)
    options = subset.Options()
    options.layout_features = FEATURES
    options.hinting = False
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.name_legacy = True
    options.notdef_outline = True
    options.recalc_timestamp = False
    subsetter = subset.Subsetter(options)
    codepoints = [c for start, end in UNICODES for c in range(start, end + 1)]
    subsetter.populate(unicodes=codepoints)
    subsetter.subset(font)

    out = pathlib.Path(__file__).resolve().parent.parent / "crates/manguebyte-gfx/fonts"
    out.mkdir(parents=True, exist_ok=True)
    target = out / "InterVariable-Latin.ttf"
    font.save(target)
    (out / "OFL.txt").write_bytes(archive.read(LICENCE_IN_ARCHIVE))
    glyphs = len(font.getGlyphOrder())
    print(f"wrote {target} ({target.stat().st_size} bytes, {glyphs} glyphs)")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
