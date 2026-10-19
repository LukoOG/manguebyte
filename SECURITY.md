# Security

Manguebyte is a young project: only its latest release on crates.io and the
`main` branch receive fixes.

## Reporting a vulnerability

Please do not open a public issue. Report it privately through GitHub:
**Security → Report a vulnerability** on
[the repository](https://github.com/alessonqueirozdev-hub/manguebyte/security/advisories/new).
If that is not possible, write to alessonqueiroz.dev@gmail.com.

Say what is affected (crate and version, or commit), how to reproduce it and
what an attacker could do with it. The aim is to answer within a week.
Once a fix is released, the advisory is published with credit to you,
unless you would rather stay anonymous.

## What counts

Manguebyte reads files and data from outside (WAV, AIFF, FLAC, Ogg, MP3,
Opus and MPEG audio, SFZ instruments, MIDI files, OSC packets, plugin
state, LV2 Turtle) and loads plugins made by others. A crash, a hang, memory
unsafety or unbounded memory use caused by such input is a security issue.
So is the Aratu Bridge accepting a connection or message it should refuse
(see [docs/ARATU_BRIDGE.md](docs/ARATU_BRIDGE.md)).

A plugin loaded on purpose doing harm is not: a hosted plugin runs with the
host's rights, as in every plugin host.
