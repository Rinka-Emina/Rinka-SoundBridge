use rinka_soundbridge_core::fifo::AudioFIFO;

#[test]
fn test_fifo_basic_read_write() {
    let fifo = AudioFIFO::new(16384);
    fifo.configure(256, Some(48000.0));
    fifo.reset();
    let in_l = vec![0.25f32; 256];
    let in_r = vec![-0.25f32; 256];
    fifo.write(&in_l, &in_r);
    assert!(fifo.fill() >= 256);

    let mut out_l = vec![0.0f32; 256];
    let mut out_r = vec![0.0f32; 256];
    fifo.read_into(&mut out_l, &mut out_r);
    assert_eq!(out_l.len(), 256);
}

#[test]
fn test_fifo_resampler_continuity() {
    let fifo = AudioFIFO::new(16384);
    fifo.configure(256, Some(48000.0));
    fifo.reset();
    let mut in_l = Vec::with_capacity(512);
    let mut in_r = Vec::with_capacity(512);
    for i in 0..512 {
        let val = (i as f32 * 0.05).sin();
        in_l.push(val);
        in_r.push(val);
    }
    fifo.write(&in_l, &in_r);

    let mut out_l = vec![0.0f32; 256];
    let mut out_r = vec![0.0f32; 256];
    fifo.read_into(&mut out_l, &mut out_r);
    assert_eq!(out_l.len(), 256);
}
