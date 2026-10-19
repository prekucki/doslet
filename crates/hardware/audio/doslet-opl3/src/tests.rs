use super::*;
#[allow(dead_code)]
mod fixture {
    include!("../../../../../tests/fixtures/opl3.rs");
}
use fixture::OPL_TONE;

fn write(opl: &mut Opl3, register: u16, value: u8, time: u64) {
    let bank = ((register >> 8) * 2) as u8;
    opl.write(bank, register as u8, time);
    opl.write(bank + 1, value, time);
}

#[test]
fn note_diagnostics_distinguish_initialization_and_both_banks() {
    let mut opl = Opl3::default();
    write(&mut opl, 0x105, 1, 0);
    write(&mut opl, 0xb0, 0, 0);
    assert!(opl.started());
    assert_eq!(opl.key_on_writes(), 0);
    write(&mut opl, 0xb0, 0x20, 0);
    write(&mut opl, 0x1b8, 0x31, 0);
    write(&mut opl, 0xb9, 0x20, 0);
    assert_eq!(opl.key_on_writes(), 2);
    #[cfg(feature = "save-state")]
    {
        use doslet_state::State;
        assert_eq!(Opl3::load(opl.save()).unwrap().key_on_writes(), 2);
    }
}

#[test]
fn adlib_detection_timers_use_guest_time_and_bank_one_is_not_a_timer() {
    let mut opl = Opl3::default();
    write(&mut opl, 4, 0x60, 0);
    write(&mut opl, 4, 0x80, 0);
    assert_eq!(opl.read(0, 0), 0);
    write(&mut opl, 2, 0xFF, 0);
    write(&mut opl, 4, 0x21, 0);
    assert_eq!(opl.read(0, 79_999), 0);
    assert_eq!(opl.read(0, 80_000), 0xC0);
    write(&mut opl, 4, 0x80, 80_000);
    assert_eq!(opl.read(0, 80_000), 0);
    assert_eq!(opl.read(0, 160_000), 0xC0); // Reset flag did not stop timer.
    write(&mut opl, 4, 0x60, 160_000);
    assert_eq!(opl.read(0, 1_000_000), 0);
    assert!(!opl.started());
    write(&mut opl, 3, 0xFF, 1_000_000);
    write(&mut opl, 4, 2, 1_000_000);
    write(&mut opl, 0x104, 0x80, 1_000_000);
    assert_eq!(opl.read(0, 1_319_999), 0);
    assert_eq!(opl.read(0, 1_320_000), 0xA0);
    for offset in [1, 2, 3] {
        assert_eq!(opl.read(offset, 1_320_000), 0xFF);
    }
}

#[test]
fn fm_samples_and_timestamps_are_identical_across_batch_partitions() {
    let mut a = Opl3::default();
    let mut b = Opl3::default();
    for (register, value) in OPL_TONE {
        write(&mut a, register, value, 0);
        write(&mut b, register, value, 0);
    }
    let mut first = Vec::new();
    let mut second = Vec::new();
    for i in 1..=100 {
        a.advance(1_000_000, i * 1_000_000, |t, sample| {
            first.push((t, sample))
        });
    }
    for i in 1..=100_000 {
        b.advance(1000, i * 1000, |t, sample| second.push((t, sample)));
    }
    assert_eq!(first, second);
    assert_eq!(first.len(), 4800);
    assert!(
        first
            .iter()
            .any(|(_, sample)| sample[0].unsigned_abs() > 100)
    );
}

#[test]
fn opl3_secondary_bank_routes_a_voice_to_right_output() {
    let mut opl = Opl3::default();
    write(&mut opl, 0x105, 1, 0);
    for (register, value) in OPL_TONE {
        write(
            &mut opl,
            register | 0x100,
            if register == 0xC0 { 0x20 } else { value },
            0,
        );
    }
    let mut peak = [0_u16; 2];
    for i in 1..=100 {
        opl.advance(1_000_000, i * 1_000_000, |_, samples| {
            for channel in 0..2 {
                peak[channel] = peak[channel].max(samples[channel].unsigned_abs());
            }
        });
    }
    assert_eq!(peak[0], 0);
    assert!(peak[1] > 100);
}
