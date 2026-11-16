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
| C | both ways (legacy) | 21000 | A-QPSK | as the spec gives | 9.6 kbit/s voice: detected and named, not decoded |

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
  `8400bps_ambe_sample` locks (181 of 219 sub-band units) and its voice frames
  show a ringback cadence of tone frames. Tone frames are not synthesised yet
  (mbelib gives silence for them): to do.
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

## Voice follow

1. The P channel's C-channel assignment signal units name the frequency and
   the aircraft: a decoder is started there at once.
2. Independently, a carrier watcher on the voice band spectrum starts a
   decoder on any new carrier of the right width, so calls are followed when
   the P channel is not being received (and on C-band).
3. A decoder that loses its carrier for a few seconds is retired and the call
   is closed in the log.

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
   SDR++'s source; not yet tried against a real SDR++ server. The server's
   radio is chosen in SDR++ itself (its remote menu is not shown here).

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

Still to do, roughly in this order:

- R and T channel bursts (C-band side).
- Tone frames synthesised (ringback is silent now).
- JAERO-style ACARS / ADS-C feeds out (JSON over UDP, SBS).
- Channel activity history (the heat map), kept across restarts; call log file.
- decsat-server's compressed link as an input; a prefs file.
- Real IF input: hide the mirror half of the band.
- CI on Actions and the portable exe (no console window).
- A test modulator crate, so the sensitivity figures are regression tests.
