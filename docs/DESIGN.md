<!-- made by CasualArclamp -->
# DecAero design

A receiver for Inmarsat Classic Aero: every channel type, voice included, with a
"voice follow" mode that watches the voice band and starts a decoder when a new
carrier appears. The look is DecSAT's.

## Decisions (Rory, 2026-11-16)

| Question | Answer |
|---|---|
| Home | Its own repo, native egui window, DecSAT crates as git dependencies where they are light |
| Bands | L-band (P channel, ground half of C voice) and C-band (R, T, aircraft half of C voice) |
| Voice codec | AMBE 4800x3600 as a Rust port in the repo (`decaero-ambe`), patent notice in NOTICE |
| IQ in | SDR++ server, plain TCP/UDP IQ, files, and DecSAT's `decsat-server` compressed link |
| Test signals | A synthetic Aero modulator first; JAERO's sample recordings; the live feed when it exists |
| Outputs | Call recordings in Ogg Opus (small), a call log, JAERO-style ACARS / ADS-C feeds, aircraft lookup from a downloaded database, channel activity history |
| GitHub | Private `CasualArclamp/DecAero`, a push per milestone |
| Layout | Mockup 4 (three columns), with the left decoder panel carrying the constellation, a vertical margin bar, an Eb/N0 trend and Signal / FEC / Voice / Call sections. See `docs/mockups/index.html` |

## Channels

| Channel | Direction | Rate (bit/s) | Modulation | Coding | Frame |
|---|---|---|---|---|---|
| P | ground to aircraft, continuous | 600, 1200 | A-BPSK | rate 1/2 K=7, interleaver 64 x 6 or 9 | 32-bit unique word, 16-bit header, 1152 coded bits, 6 signal units |
| P | same | 10500 | A-QPSK | rate 1/2 K=7, interleaver 64 x 78 | unique word on both rails, header, 178 dummy bits, 4992 coded bits, 26 signal units |
| R | aircraft to ground, burst | 600, 1200, 10500 | A-BPSK / A-QPSK | rate 1/2, 64 x 5 | one 19-octet signal unit |
| T | aircraft to ground, burst | 600, 1200, 10500 | A-BPSK / A-QPSK | rate 1/2, 64 x (5 + 3n) | 6-octet header, n 12-octet signal units |
| C | both ways, per call | 8400 | A-QPSK | rate 2/3 (punctured K=7), 16 blocks of 64 x 4 | 52-bit unique word per rail, 25 voice frames of 96 bits, 3 sub-band signal units, every 0.5 s |
| C | both ways (legacy) | 21000 | A-QPSK | rate 1/2 K=7, 27 blocks of 64 x 6 | 9.6 kbit/s voice: frame and signalling decoded (worked out from a recording), speech not (codec unknown) |

Layouts are taken from JAERO (`aerol.cpp`), which is the open reference; the
Inmarsat SDM is not public. Constants cite the JAERO function they come from.

## Found on real signals (2026-11-16)

Two of Rory's recordings (real audio IF, 192 kHz, the same signal on both WAV
channels; kept outside the repo) decode through `decaero-core`: P 600 with
54 of 54 signal units good, P 1200 with 144 of 144.

- A-BPSK behaves as MSK: squaring the signal gives two lines at
  2 fc ± Rb / 2. Their mean is twice the carrier and their spacing is the bit
  rate, which gives carrier and clock to a fraction of a hertz with no loop.
  This is the acquisition method for weak signals.
- Sampling the half-sine matched filter once a bit and turning each sample by
  a further quarter circle leaves an antipodal sequence; a data bit is the
  product of two neighbours (JAERO `DiffDecode`). Either turning sense works,
  the bits just come out inverted, and the frame decoder accepts both.
- The header's frame counter runs 0 to 3 at 600 bit/s and 0 to 7 at 1200
  bit/s (an 8 s cycle).
- `IQ/areo.wav` (625 kS/s, 8-bit, his, not committed) holds sixteen 600 bit/s P
  channels and one 1200 on a 5 kHz grid: the Rust chain (`aerorx` example)
  decodes all 17 with every CRC good, 3204 signal units, while the recording
  drifts 18 Hz in a minute. It also holds two weak 10500 bit/s carriers;
  `areo2.wav` and `areo4.wav` each hold a weak 8400 bit/s voice carrier.
- A-QPSK gives the same squared line pair, so one acquisition names all four rates.
- Weak signal, measured: the two 10500 bit/s carriers in `areo.wav` sit at about
  -2 to -3 dB Es/N0 (11 to 12 % raw bit errors) and still give 90 % and 79 % of
  their signal units with a good CRC.
- C channel: both of Rory's voice carriers are at 8 to 9 % raw bit errors, too
  weak for the rate 2/3 code (it wants about 3 dB more), but frames lock and
  sub-band call-progress units come through with good CRCs (`areo2.wav`:
  aircraft 71BE46 via ground station 82). They are the weak-signal test for
  voice; a strong call is still wanted.
- JAERO's samples: `10.5k_sample` decodes 12480 of 12480 units;
  `8400bps_ambe_sample` locks (181 of 219 sub-band units). Of its 7000 voice
  frames 1807 are speech, 5061 silence and 3 read as tone frames, but those
  three carry 4 or 5 bit errors: it holds no real tone frame.
- Tone frames are not played (silence, as mbelib): mbelib's 4800x3600 decoder
  only recognises them, and where the tone's number and level sit in the
  72 bits is not known. Its D-STAR decoder has a layout, but for a 49-bit
  frame, and a guess would put wrong tones in the recordings. A tone frame
  decoded without errors is written to the log in hex ("tone frame"), one
  line a new pattern, so the layout can be worked out from a real call.
- A demodulator block measured between 128 and 1024 bits makes no difference to
  the error rate on these weak carriers: the limit is thermal, not tracking.
- The input can be real IF (a receiver's audio output), so the front end
  takes real samples as well as IQ.

## Crates

- `decaero-ambe`: the voice decoder. Done.
- `decaero-core`: channel code, scrambler, interleaver, CRC (done); frame
  sync, frame codecs for P / R / T / C, signal units, ACARS reassembly (next).
- `decaero-dsp`: A-BPSK and A-QPSK demodulators built for low Eb/N0, the test
  modulator, the channeliser, the carrier watcher.
- `decaero-engine`: one front end thread fanning blocks out to a worker per
  channel (DecSAT's model), voice follow, recording, feeds, aircraft database.
- `apps/decaero-gui`, `apps/decaero-cli`.

## Low signal

The target is to hold lock to about 2 dB Eb/N0, where the K=7 code still
delivers. That needs: matched filtering at the exact symbol rate, a carrier
loop narrow enough for 600 bit/s (DecSAT's 250 Hz floor is far too wide),
unique-word correlation on soft symbols rather than a hard-bit compare,
frame-aided phase ambiguity resolution, and fly-wheeling over missed unique
words. Every claim is measured against the test modulator with added noise.

## Decoder figures shown (as DecSAT's DVB-S2 card)

Eb/N0 and C/N0, level, carrier offset and drift, clock error, phase scatter,
EVM / MER, **margin** over the decode limit, unique-word hits, **FEC load**
(share of the code's correcting power in use), pre-FEC BER, corrected bits,
CRC good / bad, voice frames repeated / muted, CPU.

## Constellation (Rory, 2026-11-16)

Square, redrawn at 60 frames a second, small dots.

A-BPSK is drawn as sampled, turned onto the real axis: two lumps. A-QPSK is
offset QPSK and is drawn with the offset taken out (Rory, 2026-11-18): each
decision of one rail against the next decision of the other, so it shows
plain QPSK's four corners. Drawn as sampled it was two columns of three
lumps, the other rail being caught half way between its decisions.

## Voice follow

1. The P channel's C-channel assignment signal units name the frequency and
   the aircraft: a decoder is started there at once.
2. Independently, a carrier watcher on the voice band spectrum starts a
   decoder on any new carrier of the right width, so calls are followed when
   the P channel is not being received (and on C-band).
3. A decoder that loses its carrier for a few seconds is retired and the call
   is closed in the log.

### Call progress (Rory's table, 2026-11-18)

The call progress unit (0x30), 12 octets as on the P channel and the C
channel's sub-band, from Rory's `aero_call.ods` (`su::call_progress`):

| Octet | Field |
|---|---|
| 1 | 0x30 |
| 2 to 4 | AES |
| 5 | GES |
| 6 | Q number, the call's priority (F distress, E urgent, D general), over the application reference, the call's number (0 to 7 aircraft to ground, 8 to F ground to aircraft) |
| 7 | low nibble the report type: 1 status, 2 connect, 3 test, 4 result, 5 release. High nibble: in a status report the EIRP adjustment (0 none, 1 to 3 dB up, D to F for 3 to 1 dB down); in a result report the location |
| 8 | result report only: a reserved bit, the cause's class (3 bits) and value (4 bits) |
| 11, 12 | CRC |

The log line of such a unit says it in words, and the calls table has the
last one of each call. The cause's classes and values are not named (the
table points at pages 264 and 265 of a document not held here). On the R
channel the unit is 19 octets and laid out otherwise: not read there.

## Milestones

0. Repo, codec port, channel code primitives. **Done.**
1. Frame codecs; P channel 600/1200 decoded from IQ. **Done.**
2. A-QPSK: P 10500, C 8400 with voice out; JAERO's samples. **Done**; voice not yet heard on a strong call.
3. Engine: a worker per carrier, carrier survey, deep scan, voice follow from P
   channel assignments, calls, message log. **Done.**
4. GUI, three columns. **Done** (`cargo run --release -p decaero-gui -- FILE.wav --center MHZ`).
5. Call recording to Ogg Opus. **Done** (Documents\DecAero\calls, or `--out-dir`).

6. Aircraft lookup (tar1090-db's aircraft.csv.gz, fetched on a click into the
   settings folder), state of registry from the address, All / US / Military
   filters; wheel zoom and right-drag pan on the band as in DecSAT. **Done.**

7. AVX2 and AVX-512 copies of the hot loops, picked at run time
   (`decaero-dsp/src/simd.rs`; DecSAT's block FIR and an oscillator multiply
   done the same way). Measured on the 9800X3D with `--example bench`:
   down-converter 136 to 474 M samples/s, demodulator 20.5 to 54.3 M
   samples/s, outputs unchanged. A channel costs about a quarter of a percent
   of a core. **Done.**

8. Left-drag on the band draws the box of a new decoder, which then names
   the carrier inside it (bit rate and so channel type) by itself. **Done**;
   checked headless with `--example headless -- FILE --no-follow --add HZ:WIDTH`.

9. Live network input (`decaero-engine/src/net.rs`): an SDR++ server
   (`sdrpp://HOST[:5259]`, 16 or 8 bit, zstd, retuned from the centre box) and
   plain I/Q over `tcp://`, `listen://`, `udp://`. **Done** against a stand-in
   server (`--example net_send -- FILE.wav sdrpp://127.0.0.1:5259`) built from
   SDR++'s source; tried against a real SDR++ server (an Airspy,
   2026-11-18): samples, retuning and the remote menu work. The server's
   controls for its radio are shown in a row under the top bar: SDR++'s
   `SmGui` draw list is read into widgets (`decaero-engine/src/smgui.rs`), a
   change goes back as a `COMMAND_UI_ACTION` asking for the menu in return,
   and layout steps and pop-ups are left out
   (`--example sdrpp_menu -- sdrpp://HOST:PORT [--set ID VALUE]` prints it).

10. The same radios as DecSAT (`decaero-radio`): HackRF, Airspy, ADALM-PLUTO,
    SDRplay, Fobos, RTL-SDR and UHD through DecSAT's own drivers (`decsat-io`
    as a git dependency pinned to v1.10.0), receive only, bias tee never
    switched on. A picker, rate choice and gain sliders in the top bar;
    `--radio KEY [--rate HZ]`. The engine scales its FFT with the sample
    rate and the deep scan covers the middle 1.5 MHz of a wide input.
    Bias-T is a checkbox (HackRF, Airspy, SDRplay, RTL-SDR), off unless ticked
    and applied when the radio starts; `--bias-tee`.
    **Run on the ADALM-PLUTO (2026-11-16, no antenna)**: opens in 0.4 s,
    2 MS/s with no blocks lost, retune and gain act, no decoder started on
    noise or on the DC spike in 25 s. The other radios are built but not run
    (they may be in use by other apps: ask before opening one).
    `cargo run --release -p decaero-radio --example probe -- pluto 1545.0`
    exercises a radio without the GUI.

11. CI on Actions (build and test on Windows and Linux, clippy, fmt) and a
    release workflow that builds the portable `decaero-gui.exe` (static C
    runtime, Windows GUI subsystem so no console window) into a draft release
    of a tag. `scripts/build-portable.ps1` does the same build into `exe\`.
    **Done.**

12. R and T channel bursts. `decaero-core/src/rtframe.rs` (frames, R user
    data), `decaero-dsp/src/burst.rs` (an energy detector, then the whole
    burst demodulated at once: carrier from its own squared spectrum, one
    timing offset, phase in short blocks). Every channel at 600, 1200 or
    10500 bit/s also watches for bursts while it holds no continuous signal,
    and shows as R/T once one decodes; a decoder placed by hand stays however
    quiet the channel is. On JAERO's samples: both 1200 bit/s files decode
    (four T bursts, 39 of 40 units) and the 10.5k file gives seven complete
    downlink ACARS / ADS-C messages. **R bursts have decoded only from the
    synthetic test**: none in JAERO's (MP3-coded) 10.5k sample decoded, and
    several of its weaker T bursts do not either. No C-band recording of
    Rory's has been tried.
13. A real-IF input shows and uses its upper half only (the lower is its mirror).

14. L-band correction carriers (Rory, 2026-11-21: "a decoder for it that only
    shows what's in the clear"). They are continuous BPSK carriers between the
    Aero channels on 143.5 E, by all signs the GNSS correction services that
    rent Inmarsat capacity. Their payload is encrypted, so the decoder stops
    at the clear layer: lock, the 16-bit ID and the 7-bit field after it, the
    payload's first bytes (and whether that start was seen before), and the
    usual signal and FEC figures. `decaero-core/src/corr.rs` holds the frame
    and what is known of it, `decaero-engine/src/corr.rs` the worker's path,
    `--example corrrx` the bench receiver. **Done**; see below.

## L-band correction carriers

Worked out from `areo phone assignmeant.wav` (5 MS/s, centre 1545.000 MHz);
the scripts and notes are in `research_notes/L-band 2438 baud carriers`. No
spec was found, so all of this was measured on that recording.

| Carrier (MHz) | Symbols/s | Pulse | Frame | ID seen |
|---|---|---|---|---|
| 1546.0181, 1546.6206 | 51200/21 = 2438.095 | RRC, about 0.4 | 8192 symbols, 3.36 s | 14CE (both) |
| 1546.5806 | 2438.095 | RRC | 8192 symbols | E932 |
| 1546.6006 | 25600/21 = 1219.048 | RRC | 8192 symbols, 6.72 s | 4088 |
| 1546.2232, 1547.0306 | 1200 | RRC, about 0.4 | none fixed (packets) | – |

- Framed carriers: a 64-symbol unique word, `C2B5D127` twice, uncoded. From
  its last symbol on come 8128 symbols of the Aero code (K=7, 133 / 171) with
  the encoder carried over from the frame before and flushed to zero: 4064
  bits. Bits 0 to 15 are the same in every frame of a carrier, bits 16 to 22
  are 0x7F at 2438 and 0 at 1219, then the payload, then at least 8 zero bits.
  No CRC was found (16 and 32-bit CCITT, CRC-16 and CRC-32 tried at every
  place near the end).
- The payload is white (Berlekamp-Massey gives half the length), but some
  frames on a carrier start with the same bytes as earlier ones, so messages
  repeat. The log names a repeat ("same start as at m:ss").
- The 1200 symbol/s carriers use the same code with the pair order swapped
  and runs of coded zeros between packets of varying length. They get code
  lock and figures only (`StreamCheck`).
- Squared, plain BPSK gives one line at twice the carrier (Aero gives a pair),
  and the envelope gives a line at the symbol rate: `acquire::find_bpsk`. A
  worker that finds no Aero line pair tries this before giving up. The
  survey also starts workers on carriers 2.2 to 5 kHz wide (with no Aero rate
  to look for, they go straight to this), widest first, so the few always-on
  correction carriers do not wait behind the 48-worker cap.
- A found carrier shows as searching until a frame (or a piece with code lock)
  comes, and is given up after about three frame times without one: a tone
  next to a modulated neighbour passes the line test but never gives a frame.
- On the recording, follow mode finds and locks all six (about 11 dB Eb/N0,
  BER 2e-4), and `IQ/areo.wav` has one more, a 2438 carrier at +126.8 kHz.
  Sensitivity has not been measured.

## Losing and finding sync

The demodulator's frequency loop reads only a few hertz either way at the slow
rates (about 9 Hz at 600 bit/s: a quarter of a 64-bit block is 27 ms). A break
in the samples or a burst of interference throws it further, and it then sits
off the carrier for good: the constellation is a ring and nothing decodes.
`areo.wav` has such a disturbance near 55 s that took out half its carriers.

So each worker watches itself (`Resync` in `channel.rs`): out of lock for 2.5 s
(1 s at the fast rates), it looks for the line pair again in the last seconds
of its own samples, as the first acquisition did, and moves the oscillator
there if the carrier is plain (strength 4.5) and more than 2 Hz from where the
loop is. It tries again every 4 s (1.5 s). Burst channels are left alone. The
Signalling log says "carrier found again" with how far the loop had gone.

Measured: `areo.wav` played round and round for 200 s, all 20 carriers locked
at the end (before: about 4). JAERO's 600 bit/s sample, whose carriers wander:
368 of 408 units good on its two carriers (before: 118 of 174).

Two things found on the way, both on a receiver's audio recorded at 192 kS/s:
the survey's noise floor was taken from the silence outside the audio, so the
whole audio band counted as one wide carrier (now only bins within 45 dB of
the strongest count); and the deep scan found the one strong carrier again in
every channel of its filter bank, moved by whole steps of half the channel's
sample rate (now recognised as an echo of a carrier already being decoded).

## Settings

`apps/decaero-gui/src/prefs.rs`: `key = value` lines in
`%APPDATA%\DecAero\prefs.txt` (`~/.config/DecAero/prefs.txt` elsewhere),
written a second after anything changes and readable by hand. Kept: centre
frequency, Follow voice, Scan, volume, log tab, aircraft filter, the network
input (URL, rate, format, 8 bit), the feeds, the picked radio, and for every
radio its rate and gains (`rate.KEY`, `gain.KEY.STAGE`) and the bias-T choice.

- The command line wins over the file for whatever it gives.
- Nothing is opened from the file: a remembered radio (and its bias-T) is
  only the choice shown, used when Start radio is pressed.
- An unattended `--screenshot` run neither reads nor writes the file unless
  given one of its own with `--prefs PATH`. `--no-prefs` turns it off.

## Waterfall

`apps/decaero-gui/src/band.rs`, `crates/decaero-engine/src/zoom.rs`.

- **Pixels.** The texture has one column a screen pixel and each row is a
  whole number of screen pixels high, drawn unfiltered; the band's edges, the
  marks and the grid lines are put on pixel edges or middles. Nothing is
  resampled by the graphics card, at any display scaling.
- **Columns.** Several bins under a column: the strongest (a narrow carrier
  is never lost between columns). Less than a bin a column: interpolated
  between bins, and rows averaged in time (up to 12) so the noise is not
  drawn as dashes.
- **60 rows a second.** The engine makes three rows a block (overlapping
  transforms). They reach the window in clumps, so it lets them in at the
  measured pace, sliding the picture down in whole pixels. The texture is a
  ring (as DecSAT's): a new row is a one-line upload.
- **Finer when zoomed in.** Once a pixel is narrower than one of the band's
  bins, the window tells the engine the view (`Command::Show`) and the engine
  mixes it to zero, decimates (the CIC `Ddc`, four times oversampled, droop
  undone) and transforms a stretch `sqrt(base length x pixels / view width)`
  seconds long, up to 1 s: the resolution grows smoothly with the zoom, and
  a bin stays about as wide on screen as it is tall. Levels are kept as
  densities on the band's scale, so noise and carriers keep their colours.
- **Nothing is cleared.** Every row keeps the band's own spectrum; the finer
  one is laid over it where it reaches. Zooming or panning never restarts
  the waterfall: a new zoom only adds detail once it has gathered samples,
  and one that still covers the view is kept.
- Unattended: `--view LO_HZ:HI_HZ` sets the view, and a `--screenshot` run
  prints the mean and longest frame times.

### Scales

The spectrum trace and the waterfall's colours fit what is on screen, as
DecSAT's do: the floor is the level a fifth of the trace lies under, the top
the highest peak, both followed with some inertia. The trace runs from 6 dB
under the floor to 4 dB over the peak (at least 30 dB), the colours from 4 dB
under the floor to 3 dB over the peak (at least 25 dB). The colours follow in
steps of more than 1 dB, because a new range means colouring every row again.

### I/Q recording

The **● I/Q** button records the part of the band on screen: mixed to zero,
decimated to at least four times the width shown (the CIC decimator is flat
over its middle quarter), 16-bit stereo WAV in `Documents\DecAero\iq`, named
by time, frequency and width. The level of the first block is put 12 dB under
full scale and that scale is kept. A retune, the end of the input or a
disconnect ends it (`decaero-engine/src/iqrec.rs`).

### The 21000 bit/s voice channel (9.6 kbit/s speech)

Asked for by Rory, 2026-11-18. JAERO does not receive it and nothing public
describes it, so the frame was worked out from his recording of a call
(`areo phone assignmeant.wav`, the carrier 1.34 MHz under the centre for the
first half minute), with the 8400 bit/s channel as the model:

1. Squared, the carrier shows a line pair 21000 Hz apart: offset QPSK at
   21000 bit/s. The demodulator takes it as it is (roll-off 1.0);
   `decaero-dsp --example softbits` writes its soft bits.
2. The bits repeat with a period of 10500 (half a second), and 132 of them
   are the same in every frame: a 52-bit word on each rail and 28 more bits.
3. `decaero-core --example c21probe` tried header lengths and interleaver
   widths against the Viterbi decoder: 132 bits of header and 27 blocks of
   64 x 6 at rate 1/2 leave 0.1 % channel errors (anything else, 5 % or more).
4. Descrambled, the 5184 information bits have 12 bits every 204 that hold
   12-octet signal units with good CRCs (fill-in units and call progress):
   84 spare bits, then 25 times 12 signalling bits and a 192-bit voice frame.

That is `decaero-core/src/c21frame.rs`, and it is right as far as the CRCs
say so (one call, one ground station). In the engine a carrier of this kind
is a C channel marked `wide`: named "C 21000", its signal units and call
progress shown as for 8400, its call in the calls table. Follow tries both
voice rates on an assigned channel; a box dragged wider than 12 kHz does too.
The deep scan looks for its line pair with the others (since 2026-11-19) and
starts a 24 kHz wide decoder on a find.

### The 9.6 kbit/s speech codec (2026-11-19)

Decoded: `decaero-core/src/mplpc.rs`. The codec is British Telecom's
Skyphone codec, multipulse-excited LPC, and its whole definition is public
after all: ICAO Annex 10, Volume III, Part I, Appendix 7 to Chapter 4,
"Voice encoding algorithm definition" (first edition as amended to
Amendment 80, printed pages 198 to 217; the 2007 second edition dropped
it). Encoder and decoder in Pascal, every table. Found by Rory's asking for
a search, after a night of guessing at the bits had got as far as
"syllables".

The 192-bit frame, each field most significant bit first (table A7-17):

| bits | what |
|---|---|
| 6 6 5 5 4 4 3 3 2 2 | ten reflection coefficients (tables A7-2 to A7-11) |
| 6 | (31,26) Hamming check on the upper bits of the first nine, and a parity bit |
| 8 | burst check: column parities of those 32 bits in rows of eight, XOR 10101010 |
| 2, 6 | pitch predictor: gain (0.1, 0.325, 0.6, 0.9), delay (32 + code samples) |
| 5 x 26 | five 4 ms blocks of three pulses: place 5, size 4; place 5, size 4; place 5, size 3 |

The decoder is short: the pulses, through a one-tap pitch predictor,
through a tenth-order all-pole filter, with no post-filter and nothing
smoothed between frames. A pulse's size is a sign and a level relative to a
step size that follows the levels already sent (so there is no loudness
field). Bad frames: one wrong bit among the protected ones is put right;
past that the frame before is played again, and then silence (figure A7-2).

**The sound** *(2026-11-19, asked for by Rory: "a sharp lowpass at 4 kHz (or
a proper resampler from 8k) for the 21k voice, or something else to make it
sound better")*. Two things, neither heard by me (the before and after are
in `out/plain.wav` and `out/post.wav` for him to judge):

* The GUI's player brought 8 kHz up to the card's rate by linear
  interpolation, which lets the images through (speech at 3 kHz comes out
  again at 5 kHz only 13 dB down) and droops 2.7 dB at 3 kHz. It is now a
  96-tap Kaiser-windowed sinc (`audio.rs`, `Upsampler`): flat to 3.6 kHz,
  60 dB down from 3.95 kHz. A test holds 300 to 3400 Hz within 3 % and
  their images under −54 dB. This is for every call, 8400 bit/s too.
* The 21000 bit/s decoder has an adaptive post-filter after the appendix's
  filter (`mplpc.rs`, `PostFilter`; `Decoder::with_postfilter`, which the
  engine uses; `Decoder::new` is still the appendix to the letter):
  A(z/0.55)/A(z/0.7) with tilt compensation and the level put back, G.729's
  short-term post-filter. On the recorded call the level is unchanged
  (RMS 0.0414 → 0.0415) and the spectrum of loud frames is 1.1 dB deeper
  between its peaks: mild on purpose. `mplpc_wav --plain` makes the sound
  without it.

What the appendix leaves open, settled on the recorded call:

* The coefficient tables are sines of evenly spaced angles, each output
  level the midpoint of two; the module makes them that way and a test
  holds them to printed values.
* The 26 protected bits go into the Hamming word and the matrix in the
  order of table A7-18, coefficient 1's top bit first, row by row.
* The sixth Hamming bit makes the number of ones in the 32 bits even
  (the text says "odd-parity").
* In `c21frame`'s octets the first bit sent is the top bit.

Check: of the call's 5825 frames 5416 pass every check; the rest are two
fades of about two seconds and scattered single frames. The sound is clear
speech, and a speech recogniser writes down the conversation. A Python copy
of the decoder, `scripts/c21_render.py`, agrees with the Rust one sample for
sample on the clean stretches. `decaero-core --example mplpc_wav` turns
saved frames into a WAV; the headless example's `--raw-voice DIR` saves a
recording's frames (24 octets each, 50 a second).

In the engine a 21000 bit/s channel's sound goes the way of the 8400
channel's: to the loudspeaker when listened to, and to the call's recording.

Not done: the squelch from the demodulator into the decoder.

### Fax and data calls on the voice channel (2026-11-19)

Asked for by Rory. Not decoded, and it cannot be yet: what the voice
channel's frames hold during a fax or data call is not in ICAO Annex 10
(which calls the field "transparent data (voice)" and stops there). It is in
ARINC Characteristic 741 Part 2: "TIF transmission frame structure" tables
for 300 to 9600 bit/s and a decoder decision figure, seen only as a contents
list; the document is sold, and no copy was found. A COMSAT patent
(US 5,802,154) says Aero fax is demodulated on the ground and its bits sent
with line-state codes, as on Inmarsat-B and -M, so no modem tones are to be
expected in the frames. And there is no recording of such a call to work
from. Search notes: `F:/codebases/research_notes/Inmarsat Aero voice codec
frame/circuit_mode_data.md`.

What is done, so that the first such call caught is not lost:

* The call announcement on the P channel (type 0x20, Annex 10 figure A2-8)
  is read: service (telephone; data through a modem, which includes fax;
  data to a digital network) and the modem or speed (V.22, V.22 bis, V.32,
  V.23, Group 3 fax; 1200 to 9600 bit/s). It is in the log line and, matched
  by aircraft, in the Call panel ("Service"). Only calls from the ground are
  announced there: an aircraft's own request goes up on the R channel.
  (Type 0x21, which JAERO's list calls the announcement, is the call
  information unit.)
* On a 21000 bit/s channel, two seconds of clean signal (under 2 % channel
  errors) with almost no frame passing the speech codec's checks marks the
  call as data: the Voice panel says so, and its frames, from the call's
  start (up to a minute held back), go to a `21k` folder beside the calls
  (`voice21k_UNIXTIME_OFFSET.bin`, 24 octets a frame). Speech calls' frames
  are not kept. The 8400 bit/s channel has no such check (its codec frames
  carry no check that data would fail as surely).

Next, with a saved data call or the ARINC tables: the frame layout, then
fax pages from the T.4 bits.

### Tuning

The centre frequency is DecSAT's big digits (`freq_display.rs`). On a live
input a middle-button drag on the band tunes too. Every waterfall row keeps
the tuning it was taken at and is drawn moved along by the difference, so a
retune shifts the history instead of clearing it; where an old row does not
reach, the picture is dark. For about half a second after a retune the rows
are of samples still on their way from the radio at the old tuning, and sit
in the wrong place. A retune of 100 Hz or more on a live input stops the
decoders (the carriers have all moved) and the scan starts them again.
`--step SECS:HZ` with `--screenshot` tries a retune unattended.

## Activity history and call log

`apps/decaero-gui/src/activity.rs`. Once a second every decoder that holds
lock (a burst channel: one that has just decoded a burst) adds to the minute
now running, on a row for its frequency (rounded to the 2.5 kHz raster) and
mode. The Activity tab under the band draws it as a heat map, frequencies down
and UTC time across, over 1 h, 6 h, 24 h or 7 d; the colour is the share of
each column's time in use, on a square-root scale so a burst or a short call
still shows.

- A live input's history is kept in `activity.txt` beside the settings:
  `minute hertz mode seconds` lines, added as each minute ends and when the
  input stops. Reading it sums a minute written in parts, drops what is older
  than 8 days and then writes the file afresh.
- A recording gets a history of its own, timed as if it began when opened and
  laid out by the recording's clock, and not kept, so replaying a test file
  does not fill the real history. `--activity FILE` keeps it all the same.
- Always-on P channels cost about 25 bytes a minute each: twenty of them over
  8 days is some 6 MB.
- Call log: `calls.csv` in the recordings folder, a line as each recording is
  closed (date, UTC time, seconds, MHz, offset, AES, aircraft, GES, file).

## Feeds out

`decaero-engine/src/feed.rs`, after JAERO (`mainwindow.cpp`, `sbs1.cpp`). One
thread owns the sockets.

- **ACARS as JSON over UDP**: one datagram a message in JAERO's "JSONdump"
  layout (`app`, `isu.acars`, `isu.src`/`dst`, `qno`, `refno`, `t`, `station`),
  to any number of `host:port` collectors. `app.name` is "DecAero".
- **ADS-C positions as SBS**: a BaseStation `MSG,3` line for every report with
  a position, over TCP; `listen:PORT` (a map connects) or `host:port` (we
  connect, retried every 5 s). Fields as JAERO sends them: time taken, time
  now, flight, altitude, ground speed, track, latitude, longitude, vertical rate.
- ADS-C groups are read in `decaero-core/src/adsc.rs` (ARINC 622 check, then
  JAERO's group layouts) and also shown in words under the message in the log.
- Set from the Feeds window, or `--json-udp`, `--sbs`, `--station` on the GUI
  and the headless runner; the GUI remembers them (see Settings).
- Not done: a report split over several ACARS blocks is not put back together
  (its check fails, so it is passed on as text only); the ground's ADS-C
  requests on the P channel are not read; libacars-style decoded JSON is not added.
- Checked on JAERO's 10.5k burst sample: six messages out as JSON, three
  position lines out as SBS, to a local listener.

Still to do, roughly in this order:

- Tone frames played: needs a recording with real ones (see above).
- decsat-server's compressed link as an input.
- A test modulator crate, so the sensitivity figures are regression tests.
