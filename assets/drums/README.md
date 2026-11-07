# Drum samples

18 one-shots from **Gogodze Phu Vol II** by Karoryfer Samples, released
under CC0 1.0 (public domain; see `LICENSE`, copied from the source).

- Source: <https://github.com/sfzinstruments/karoryfer.gogodze-phu-vol-ii>,
  commit `69a0274cdc39c99c3098cde1bc3789690de86d62`, folder `Samples/`
- Kit page: <https://shop.karoryfer.com/pages/free-gogodze-phu-vol-ii>
- Files are unchanged: 16-bit mono WAV at 44.1 kHz, in the same folders and
  with the same names as in the source.

| file                         | hit                                   |
|------------------------------|---------------------------------------|
| `kick_mic/ks_vl*.wav`        | kick, velocity layers 3, 5, 6 (×2)    |
| `snare_mic/sc_vl*.wav`       | snare centre, layers 3, 5, 6 (×2)     |
| `snare_mic/se_vl4_rr1.wav`   | snare edge                            |
| `snare_mic/ss_vl3_rr1.wav`   | sidestick                             |
| `front_mic/hc_vl*.wav`       | closed hi-hat                         |
| `front_mic/hl_vl3_rr1.wav`   | loose hi-hat                          |
| `front_mic/ho_vl3_rr1.wav`   | open hi-hat                           |
| `front_mic/th`, `tm`, `tl`   | high, mid and low tom                 |

The demo song (`src/demo.rs`) builds them into a sliced two-bar break at
load time; they're compiled into the program.
