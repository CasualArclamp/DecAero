// made by CasualArclamp
//! The 9.6 kbit/s speech codec of the 21000 bit/s C channel: British
//! Telecom's multipulse-excited LPC (the "Skyphone" codec), decoder only.
//!
//! Written from its definition in ICAO Annex 10, Volume III, Part I,
//! Appendix 7 to Chapter 4, "Voice encoding algorithm definition" (first
//! edition, as amended to Amendment 80). Section and table numbers below
//! are that appendix's.
//!
//! A frame is 192 bits for 20 ms of speech at 8 kHz, each field most
//! significant bit first (table A7-17):
//!
//! | bits | what |
//! |---|---|
//! | 6 6 5 5 4 4 3 3 2 2 | ten reflection coefficients |
//! | 6 | Hamming check on 26 of those bits, and a parity bit |
//! | 8 | burst check: column parities of the 32 bits above it |
//! | 2, 6 | pitch predictor: gain, delay (32 to 95 samples) |
//! | 5 x 26 | five 4 ms blocks of three pulses: place 5, size 4; 5, 4; 5, 3 |
//!
//! The decoder: the pulses, through a one-tap pitch predictor, through a
//! tenth-order all-pole filter. No post-filter and no smoothing between
//! frames. The pulses' sizes are relative to a step size that follows the
//! sizes already sent, so it has no field of its own.
//!
//! [`Decoder::with_postfilter`] adds what the appendix leaves out and
//! later speech codecs made usual: an adaptive post-filter ([`PostFilter`])
//! that pushes the coding noise down between the formants, where the ear
//! hears it. It is not part of the appendix's decoder; [`Decoder::new`] is
//! that decoder to the letter.

/// Octets in a frame.
pub const FRAME_BYTES: usize = 24;
/// Samples a frame makes (8 kHz).
pub const FRAME_SAMPLES: usize = 160;

/// Bits of each reflection coefficient's code.
const WIDTHS: [usize; 10] = [6, 6, 5, 5, 4, 4, 3, 3, 2, 2];
/// How many of each code's top bits the Hamming code covers (table A7-18).
const PROTECTED: [usize; 10] = [6, 5, 3, 3, 2, 2, 2, 2, 1, 0];
/// Table A7-1's decision levels are the sines of evenly spaced angles
/// between these two (radians), one pair a coefficient; the output levels
/// (tables A7-2 to A7-11) are the midpoints of neighbouring decisions.
/// This reproduces every printed value to the last digit or one off it.
const ANGLES: [(f64, f64); 10] = [
    (-0.85, 1.46),
    (-1.32, 0.79),
    (-0.67, 0.90),
    (-1.08, 0.50),
    (-0.61, 0.69),
    (-0.78, 0.36),
    (-0.48, 0.67),
    (-0.68, 0.48),
    (-0.44, 0.81),
    (-0.52, 0.44),
];
/// Table A7-12.
const PITCH_GAIN: [f32; 4] = [0.1, 0.325, 0.6, 0.9];
/// The least pitch delay (table A7-13).
const MIN_DELAY: usize = 32;
/// Table A7-14: sizes of the first and second pulse of a block.
const SIZE_4: [f32; 8] = [0.1284, 0.3881, 0.6568, 0.9424, 1.256, 1.618, 2.069, 2.733];
/// Table A7-15: sizes of the third.
const SIZE_3: [f32; 4] = [0.2451, 0.7560, 1.344, 2.152];
/// How each size moves the step size (section 5.1.3.3).
const ADAPT_4: [f32; 8] = [0.75, 0.875, 0.875, 0.875, 1.0, 1.25, 1.5, 2.0];
const ADAPT_3: [f32; 4] = [0.875, 0.875, 1.0, 1.5];
/// The second and third pulses are sent larger by these.
const ADJUST: [f32; 3] = [1.0, 1.0 / 0.625, 1.0 / 0.375];
/// The mask on the burst check bits (section 4.10.3.2).
const BURST_MASK: u8 = 0b1010_1010;
/// Samples in a block of three pulses.
const BLOCK: usize = 32;
/// The codec's samples are on a 13-bit scale.
const TO_PCM: f32 = 8.0;

/// What the decoder made of a frame's checks (figure A7-2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// Decoded as sent (a single wrong bit put right counts).
    Voice,
    /// Damaged: the frame before it was played again.
    Repeat,
    /// Damaged past repeating: silence.
    Mute,
}

/// A frame's fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fields {
    k: [u8; 10],
    hamming: u8,
    burst: u8,
    gain: u8,
    delay: u8,
    /// Fifteen pulses: (place, size code).
    pulses: [(u8, u8); 15],
}

fn fields(frame: &[u8; FRAME_BYTES]) -> Fields {
    let mut at = 0;
    let mut take = |n: usize| {
        let mut v = 0u8;
        for _ in 0..n {
            v = v << 1 | (frame[at / 8] >> (7 - at % 8) & 1);
            at += 1;
        }
        v
    };
    let mut k = [0u8; 10];
    for (k, w) in k.iter_mut().zip(WIDTHS) {
        *k = take(w);
    }
    let hamming = take(6);
    let burst = take(8);
    let gain = take(2);
    let delay = take(6);
    let mut pulses = [(0u8, 0u8); 15];
    for (i, p) in pulses.iter_mut().enumerate() {
        *p = (take(5), take(if i % 3 == 2 { 3 } else { 4 }));
    }
    Fields {
        k,
        hamming,
        burst,
        gain,
        delay,
        pulses,
    }
}

/// The 26 protected bits, coefficient 1's top bit first (table A7-18).
fn protected(k: &[u8; 10]) -> u32 {
    let mut word = 0u32;
    for i in 0..10 {
        word = word << PROTECTED[i] | u32::from(k[i] >> (WIDTHS[i] - PROTECTED[i]));
    }
    word
}

/// The protected bits put back into the coefficients' codes.
fn unprotect(k: &mut [u8; 10], word: u32) {
    let mut left = 26;
    for i in 0..10 {
        left -= PROTECTED[i];
        let low = WIDTHS[i] - PROTECTED[i];
        let top = (word >> left) as u8 & ((1 << PROTECTED[i]) - 1) as u8;
        k[i] = top << low | (k[i] & ((1 << low) - 1) as u8);
    }
}

/// Where in the Hamming code word (1 to 31) each protected bit sits: the
/// places that are not powers of two, in order (section 4.10.3.1).
fn place(i: usize) -> u32 {
    let mut at = 2;
    for _ in 0..=i {
        at += 1;
        while at & (at - 1) == 0 {
            at += 1;
        }
    }
    at
}

/// The five check bits of the protected bits (section 4.10.3.1).
fn hamming(word: u32) -> u8 {
    let mut ham = 0;
    for i in 0..26 {
        if word >> (25 - i) & 1 == 1 {
            ham ^= place(i);
        }
    }
    ham as u8
}

/// The burst check for these bits: the column parities of the 26 protected
/// bits and the six Hamming bits in rows of eight, masked.
fn burst(word: u32, hamming: u8) -> u8 {
    let all = word << 6 | u32::from(hamming);
    let mut b = 0u8;
    for row in all.to_be_bytes() {
        b ^= row;
    }
    b ^ BURST_MASK
}

/// The decoder, for one call.
pub struct Decoder {
    levels: [Vec<f32>; 10],
    /// The step size of the pulses and whether the last was a largest one.
    step: f32,
    at_top: bool,
    /// The pitch predictor's past: the excitation, newest last.
    past: [f32; MIN_DELAY + 63 + FRAME_SAMPLES],
    /// The filter's last ten outputs, newest first.
    out: [f32; 10],
    last: Option<Fields>,
    /// Set when a frame was bad: the next bad one is not repeated.
    squelch: bool,
    post: Option<PostFilter>,
}

/// An adaptive post-filter for the decoder's sound, of the kind of ITU-T
/// G.729's short-term post-filter (clause 4.2.2 there; the idea is Chen and
/// Gersho's, 1987): the frame's own synthesis filter 1/A(z), flattened,
///
/// ```text
/// H(z) = A(z/0.55) / A(z/0.7) · (1 + t·z⁻¹)
/// ```
///
/// which keeps the formant peaks and lowers the valleys between them by a
/// few dB, where the multipulse coding noise is not masked by the speech.
/// The last factor takes out the low-pass tilt the first leaves (`t` from
/// the first reflection coefficient of its impulse response), and the level
/// is then brought back to the input's, block by block, smoothly.
struct PostFilter {
    /// The decoder's output before this block, newest first.
    x: [f32; 10],
    /// This filter's all-pole part's output, newest first.
    y: [f32; 10],
    /// The tilt filter's last input.
    tilt_in: f32,
    gain: f32,
}

const POST_NUM: f32 = 0.55;
const POST_DEN: f32 = 0.7;

impl PostFilter {
    fn new() -> Self {
        Self {
            x: [0.0; 10],
            y: [0.0; 10],
            tilt_in: 0.0,
            gain: 1.0,
        }
    }

    /// One block of the decoder's output `s`, made with the filter taps
    /// `taps` (s[n] = e[n] + Σ taps[i]·s[n−1−i]), filtered in place.
    fn block(&mut self, taps: &[f32; 10], s: &mut [f32]) {
        let mut num = *taps;
        let mut den = *taps;
        let (mut a, mut b) = (POST_NUM, POST_DEN);
        for i in 0..10 {
            num[i] *= a;
            den[i] *= b;
            a *= POST_NUM;
            b *= POST_DEN;
        }
        // The tilt of A(z/0.55)/A(z/0.7): from 20 samples of its impulse
        // response, as G.729 does.
        let mut h = [0f32; 20];
        for n in 0..20 {
            let mut v = if n == 0 { 1.0 } else { 0.0 };
            if (1..=10).contains(&n) {
                v -= num[n - 1];
            }
            for i in 0..10.min(n) {
                v += den[i] * h[n - 1 - i];
            }
            h[n] = v;
        }
        let r0: f32 = h.iter().map(|v| v * v).sum();
        let r1: f32 = h.windows(2).map(|w| w[0] * w[1]).sum();
        let k = if r0 > 0.0 { -r1 / r0 } else { 0.0 };
        let tilt = k * if k < 0.0 { 0.9 } else { 0.2 };

        let energy_in: f32 = s.iter().map(|v| v * v).sum();
        let mut energy_out = 0f32;
        for v in s.iter_mut() {
            let x = *v;
            let fir = x - num.iter().zip(&self.x).map(|(t, p)| t * p).sum::<f32>();
            let y = fir + den.iter().zip(&self.y).map(|(t, p)| t * p).sum::<f32>();
            self.x.copy_within(..9, 1);
            self.x[0] = x;
            self.y.copy_within(..9, 1);
            self.y[0] = y;
            let out = y + tilt * self.tilt_in;
            self.tilt_in = y;
            energy_out += out * out;
            *v = out;
        }
        // The level put back to the input's, moving to it over about 10 ms.
        let want = if energy_out > 1e-9 {
            (energy_in / energy_out).sqrt().min(4.0)
        } else {
            1.0
        };
        for v in s.iter_mut() {
            self.gain += 0.0125 * (want - self.gain);
            *v *= self.gain;
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        let levels = std::array::from_fn(|i| {
            let (lo, hi) = ANGLES[i];
            let n = 1usize << WIDTHS[i];
            let edge = |j: usize| (lo + (hi - lo) * j as f64 / n as f64).sin();
            (0..n)
                .map(|j| ((edge(j) + edge(j + 1)) / 2.0) as f32)
                .collect()
        });
        Self {
            levels,
            step: 1.0,
            at_top: false,
            past: [0.0; MIN_DELAY + 63 + FRAME_SAMPLES],
            out: [0.0; 10],
            last: None,
            squelch: false,
            post: None,
        }
    }

    /// The decoder with the adaptive post-filter on its output: what the
    /// apps play. See [`PostFilter`].
    pub fn with_postfilter() -> Self {
        Self {
            post: Some(PostFilter::new()),
            ..Self::new()
        }
    }

    /// The checks of figure A7-2: the frame to play (with a wrong bit put
    /// right where that can be done), or what to do in its place.
    fn judge(&mut self, mut f: Fields) -> (FrameKind, Fields) {
        let word = protected(&f.k);
        let syndrome = hamming(word) ^ (f.hamming & 31);
        // The sixth bit makes the number of ones in the code word even
        // (seen on the air; the text calls it odd parity).
        let odd = (word.count_ones() + u32::from(f.hamming).count_ones()) % 2 == 1;
        let columns = (burst(word, f.hamming) ^ f.burst).count_ones();
        // Wrong check bits with the parity right: two bits are wrong.
        let double = syndrome != 0 && !odd;
        // One wrong bit: put it right, and see that the burst check agrees.
        let mut corrected = true;
        if syndrome != 0 && odd {
            let mut word = word;
            let mut ham = f.hamming;
            match (0..26).find(|&i| place(i) == u32::from(syndrome)) {
                Some(i) => word ^= 1 << (25 - i),
                None => ham ^= syndrome,
            }
            corrected = burst(word, ham) == f.burst;
            unprotect(&mut f.k, word);
        }
        let good = match columns {
            0 if double => false,
            0 => syndrome == 0 || !self.squelch,
            1 if double => false,
            1 if syndrome == 0 => !self.squelch,
            1 => corrected,
            _ => false,
        };
        if good {
            self.squelch = false;
            return (FrameKind::Voice, f);
        }
        let repeat = !self.squelch && columns <= 3;
        self.squelch = true;
        match self.last {
            Some(last) if repeat => (FrameKind::Repeat, last),
            _ => (FrameKind::Mute, f),
        }
    }

    /// A pulse's size from its code, moving the step size on (section
    /// 5.1.3.3). `index` is the pulse's number in its block.
    fn size(&mut self, code: u8, index: usize) -> f32 {
        let (sizes, adapt): (&[f32], &[f32]) = if index < 2 {
            (&SIZE_4, &ADAPT_4)
        } else {
            (&SIZE_3, &ADAPT_3)
        };
        let i = usize::from(code) & (sizes.len() - 1);
        let sign = if usize::from(code) >= sizes.len() {
            -1.0
        } else {
            1.0
        };
        let value = sign * sizes[i] * self.step / ADJUST[index];
        self.step = self.step.powf(0.98);
        if i == sizes.len() - 1 {
            // Two largest sizes running: the step grows twice.
            if self.at_top {
                self.step *= adapt[i];
            }
            self.at_top = true;
        } else {
            self.at_top = false;
        }
        self.step = (self.step * adapt[i]).clamp(0.5, 512.0);
        value
    }

    /// Decode one frame (24 octets, the first bit sent in the top bit of
    /// the first) to 20 ms of sound.
    pub fn decode(
        &mut self,
        frame: &[u8; FRAME_BYTES],
        pcm: &mut [i16; FRAME_SAMPLES],
    ) -> FrameKind {
        let (kind, mut f) = self.judge(fields(frame));
        if kind == FrameKind::Voice {
            self.last = Some(f);
        }
        if kind == FrameKind::Mute {
            // The step size runs on a middle size, and nothing is played
            // (sections 5.1.3.2.1 and 5.1.3.3.1).
            for (i, p) in f.pulses.iter_mut().enumerate() {
                p.1 = if i % 3 == 2 { 0b010 } else { 0b0100 };
            }
        }
        // Reflection coefficients to the filter's taps (section 4.6).
        let mut taps: [f32; 10] = std::array::from_fn(|i| self.levels[i][usize::from(f.k[i])]);
        for i in 1..10 {
            let alpha = taps;
            for j in 0..i {
                taps[j] = alpha[j] - taps[i] * alpha[i - 1 - j];
            }
        }
        let gain = PITCH_GAIN[usize::from(f.gain)];
        let delay = MIN_DELAY + usize::from(f.delay);
        let keep = self.past.len() - FRAME_SAMPLES;
        self.past.copy_within(FRAME_SAMPLES.., 0);
        for block in 0..5 {
            let mut pulses = [0f32; BLOCK];
            for index in 0..3 {
                let (place, code) = f.pulses[block * 3 + index];
                let size = self.size(code, index);
                if kind != FrameKind::Mute {
                    pulses[usize::from(place)] = size;
                }
            }
            let mut sound = [0f32; BLOCK];
            for (n, pulse) in pulses.iter().enumerate() {
                let at = keep + block * BLOCK + n;
                let x = gain * self.past[at - delay] + pulse;
                self.past[at] = x;
                let y = x + taps.iter().zip(&self.out).map(|(t, o)| t * o).sum::<f32>();
                self.out.copy_within(..9, 1);
                self.out[0] = y;
                sound[n] = y;
            }
            if let Some(post) = &mut self.post {
                post.block(&taps, &mut sound);
            }
            for (n, y) in sound.iter().enumerate() {
                pcm[block * BLOCK + n] = (y * TO_PCM).clamp(-32768.0, 32767.0) as i16;
            }
        }
        kind
    }
}

/// A frame from its fields, with its check bits made: for tests.
#[cfg(test)]
fn pack(k: [u8; 10], gain: u8, delay: u8, pulses: [(u8, u8); 15]) -> [u8; FRAME_BYTES] {
    let word = protected(&k);
    let mut ham = hamming(word);
    if (word.count_ones() + u32::from(ham).count_ones()) % 2 == 1 {
        ham |= 32;
    }
    let mut bits = Vec::with_capacity(192);
    let mut put = |v: u8, n: usize| bits.extend((0..n).rev().map(|i| v >> i & 1));
    for (v, w) in k.iter().zip(WIDTHS) {
        put(*v, w);
    }
    put(ham, 6);
    put(burst(word, ham), 8);
    put(gain, 2);
    put(delay, 6);
    for (i, (place, code)) in pulses.iter().enumerate() {
        put(*place, 5);
        put(*code, if i % 3 == 2 { 3 } else { 4 });
    }
    let mut frame = [0u8; FRAME_BYTES];
    for (i, b) in bits.iter().enumerate() {
        frame[i / 8] |= b << (7 - i % 8);
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> [u8; FRAME_BYTES] {
        let mut pulses = [(0u8, 0u8); 15];
        for (i, p) in pulses.iter_mut().enumerate() {
            *p = ((i * 7 % 32) as u8, (i * 3 % 8) as u8);
        }
        pack([40, 20, 16, 12, 8, 7, 4, 3, 2, 1], 2, 25, pulses)
    }

    #[test]
    fn levels_are_the_printed_ones() {
        let d = Decoder::new();
        // First and last of tables A7-2, A7-5 and A7-11, and one inside.
        for (i, j, printed) in [
            (0, 0, -0.7391278),
            (0, 63, 0.9915497),
            (0, 27, 0.1420724),
            (3, 0, -0.8697892),
            (3, 31, 0.4574769),
            (9, 0, -0.3866179),
            (9, 3, 0.3123044),
        ] {
            assert!((d.levels[i][j] - printed).abs() < 3e-7, "{i} {j}");
        }
    }

    #[test]
    fn hamming_places_skip_the_powers_of_two() {
        let places: Vec<u32> = (0..26).map(place).collect();
        assert_eq!(&places[..5], &[3, 5, 6, 7, 9]);
        assert_eq!(places[25], 31);
    }

    #[test]
    fn a_frame_unpacks_to_its_fields() {
        let f = fields(&example());
        assert_eq!(f.k, [40, 20, 16, 12, 8, 7, 4, 3, 2, 1]);
        assert_eq!((f.gain, f.delay), (2, 25));
        assert_eq!(f.pulses[4], (28, 4));
        let mut d = Decoder::new();
        assert_eq!(d.judge(f).0, FrameKind::Voice);
    }

    #[test]
    fn one_wrong_bit_is_put_right() {
        let good = fields(&example());
        // Every protected bit and every Hamming bit, one at a time.
        for bit in (0..11).chain(12..15).chain(17..20).chain(40..46) {
            let mut frame = example();
            frame[bit / 8] ^= 0x80 >> (bit % 8);
            let mut d = Decoder::new();
            let (kind, f) = d.judge(fields(&frame));
            assert_eq!(kind, FrameKind::Voice, "bit {bit}");
            assert_eq!(f.k, good.k, "bit {bit}");
        }
    }

    #[test]
    fn a_burst_repeats_then_mutes() {
        let mut d = Decoder::new();
        let mut pcm = [0i16; FRAME_SAMPLES];
        assert_eq!(d.decode(&example(), &mut pcm), FrameKind::Voice);
        assert!(pcm.iter().any(|&s| s != 0));
        // Two columns of the burst check wrong.
        let mut bad = example();
        bad[5] ^= 0b11;
        assert_eq!(d.decode(&bad, &mut pcm), FrameKind::Repeat);
        assert_eq!(d.decode(&bad, &mut pcm), FrameKind::Mute);
        assert_eq!(d.decode(&example(), &mut pcm), FrameKind::Voice);
        // All zeros (a long fade): the mask makes four columns fail.
        assert_eq!(d.decode(&[0; FRAME_BYTES], &mut pcm), FrameKind::Mute);
    }

    #[test]
    fn the_step_size_follows_the_pulses() {
        let mut d = Decoder::new();
        // Largest sizes make it grow, smallest shrink, within its bounds.
        for _ in 0..40 {
            d.size(7, 0);
        }
        assert_eq!(d.step, 512.0);
        for _ in 0..200 {
            d.size(0, 0);
        }
        assert_eq!(d.step, 0.5);
    }
}
