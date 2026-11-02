# ADR 0025: The space around an organ's pipes

Status: accepted, 2026-11-01

## Context

Caecilia, a pipe organ built on Manguebyte, renders every sounding pipe
as its own mono signal from a `PartialBank`. Until now it summed the pipes
into one point and sent that through a reverb. That sounds synthetic: a
real organ speaks from many places. Each pipe stands at its own spot on a
windchest, so the notes of a chord come from different directions and
distances; the divisions stand in different parts of the case; the room
answers with discrete early reflections before its reverberant tail; and
an enclosed division speaks through swell shutters that the organist
opens and closes with a pedal.

The constraints are those of ADR 0002 and ADR 0003: nothing on the audio
thread allocates, locks or waits, and state is polled. A large organ has
thousands of pipes, of which a few hundred sound at once, and placements
change rarely (an editor moving a pipe or a listener). Output promised the
same bit for bit whatever the block size must stay so: the framework's
tests rely on it, and Caecilia renders offline as well as live.

JUCE has nothing of this in its modules: `juce_dsp` has a panner and a
convolution, no placement of many sources, no early reflections, no swell
box. The tail is our `Convolution`, already done.

## Decision

**1. One module, `manguebyte_dsp::space`, of three blocks** on shared
geometry: `Position` in metres in one frame of reference; `Speakers`, one
azimuth per output channel; `Listener`, the listening point and the laws
that turn a source into a `Placement` (pan, delay, distance gain, air
absorption); `Chest`, which lays a rank out across a windchest
(chromatic, C and C♯ sides with the largest pipes outside or in the
middle, mitre, with heights that can follow the pipes' lengths).

**2. `Windchest`: accumulation rings, not a delay line per pipe.** There
is one ring per output channel. Adding a pipe's block filters it (the air)
and runs it through the taps of its fractional delay, then writes it,
times each channel's gain, ahead into the rings at `now + delay`; one call
reads the block that is due and clears it. A pipe holds its placement and
four numbers of state, and costs time only when it sounds. A delay line
per pipe would cost a line of the longest delay for each of thousands of
pipes, and a read per channel per pipe per sample all the same.

The fraction of a sample is made before the scatter, once per pipe and
not once per channel: four Lagrange taps over the pipe's own last samples
(three of history), then whole-sample writes. Writing with interpolation
weights ("splatting") would cost four writes per channel per sample
instead of one, and for a delay that is constant over a block it is the
same filter.

**The rings hold 64-bit fixed-point sums.** A cell of a ring gets its
contributions from different blocks depending on where the blocks begin,
so floating-point sums would change with the block size in their last
bits. Each contribution is rounded to a multiple of 2^-32 (by adding
`1.5·2^52`, which rounds ties to even on every processor and vectorises)
and added as an integer, with wrapping. Integer sums do not depend on
their order: the output is the same bit for bit whatever the block size
and whatever order the pipes are added in, which also leaves the door open
to adding pipes from several threads into rings of their own and summing
those. The rounding, at most 2^-33 a contribution, is far below what the
`f32` output can hold.

**Moving a pipe is a 20 ms crossfade** from its old placement to the new
one, each with its own delay, gains and filter, linear in gain since the
two are the same signal; a pipe that is not sounding moves at once. A
glide of the delay instead would bend the pitch while it moves (a Doppler
shift set by the glide's length, not by any speed), and gains alone could
not move the delay.

**The air's absorption is a one-pole low-pass** whose cutoff matches the
air's loss below it: a loss of `a·f²` decibels per metre over `r` metres
is what a one-pole at `sqrt(10 / ln 10 / (a·r))` takes well below its
cutoff. `a` comes from the loss at 8 kHz, 0.077 dB/m by default, about
what ISO 9613-1 gives at 20 °C and 70 % humidity. It is an approximation
of the physics, documented as such, not an implementation of the
standard. The filter is the topology-preserving one-pole, written so that
its recursion is one multiplication and one addition long, since that
recursion is most of what a pipe costs.

**Panning** is pairwise with the sine law on the angle between the two
nearest loudspeakers (stereo is the pair at ±30°), so the squares of the
gains always add up to one; the distance gain is `reference / r` with `r`
held above a nearest distance.

**3. `EarlyReflections`: image sources of a rectangular room** (J. B.
Allen and D. A. Berkley, JASA 65, 1979). `Reflections::compute` lists the
paths up to an order (0 to 6; order 3 is 62 paths), each with its delay,
its gain (the walls' `sqrt(1 − absorption)` times the distance gain), its
pan from the image's azimuth, and one low-pass that lumps the walls'
optional one-poles and the air by the same low-frequency match as above.
`EarlyReflections` plays them from one mono input, a division's bus,
reading the input's history at each path's delay (gathering, in a fixed
order, so the output does not depend on the blocks) and adding into the
output channels. A new set crossfades in over 20 ms. The direct path is
left out by default, since the windchest gives it. The reverberant tail
stays the `Convolution`, fed from the same sound, with a measured
response's own early part taken off by `keep_tail`.

**4. `SwellBox`: a `Processor` like `Gain` and `OnePole`.** One opening,
0 to 1, sets a level linear in decibels (−30 dB closed unless set) and a
one-pole cutoff linear in octaves (1 kHz closed to 20 kHz open unless
set), both worked out sample by sample from an opening that glides over
50 ms, so the pedal never steps.

## Consequences

- The output of every block is the same bit for bit however the host cuts
  the blocks, for all three, and for the windchest whatever order the
  pipes are added in; the tests check it with block sizes from 1 up.
- On a busy Intel Core i3-1215U, 300 sounding pipes of 2 000 placed, into
  stereo in blocks of 128 frames at 48 kHz, take about 0.22 to 0.28 ms a
  block (8 to 11 % of the block's time); about three times that while
  every one of them crossfades to a new place. Early reflections of order
  3 take about 0.04 ms a block, order 6 (376 paths) 0.21 to 0.25 ms; a
  stereo swell box under 0.01 ms (`examples/space_speed.rs`).
- A pipe that stops being added drops the three samples left in its taps,
  and starts from silence when it sounds again; the organ fades its pipes
  out itself.
- The reflections are those of one point per division, its bus; the pipes'
  own places shape the direct sound only.
- The space is `f32` only, as the partial bank is.

## Alternatives not taken

- **A delay line per pipe**, read at its delay: memory of the longest
  delay for every pipe, and as many reads.
- **Floating-point rings.** Cheaper by a conversion, but the order in
  which a cell's contributions arrive changes with the block boundaries,
  and with it the last bits of the output.
- **Splatting the fractional delay into every channel**, four weighted
  writes per channel per sample: the same filter for a delay that holds
  over a block, at up to four times the writes.
- **Gliding the delay when a pipe moves**: a pitch bend while it moves,
  with the amplitude changing as the samples bunch up or spread out.
- **Feedback delay networks or a statistical model for the early part.**
  They give a plausible density, not reflections whose times and
  directions follow from the room and the positions, which is what lets
  a listener and the divisions be moved.
- **Frequency-dependent wall absorption by octave bands** (a filter bank
  per path). Each path's loss is one low-pass here; a measured room's
  colour comes from the convolution's tail.
- **A swell law even in open area** (sound power proportional to the
  shutters' opening): physically closer, but most of its change sits in
  the first tenth of the pedal's travel.
