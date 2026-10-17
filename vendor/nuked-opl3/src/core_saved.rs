// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct OplCoreSlot {
    #[prost(uint64, tag = "1")]
    channel: u64,
    #[prost(uint64, tag = "2")]
    mod_idx: u64,
    #[prost(sint32, tag = "3")]
    prout: i32,
    #[prost(uint32, tag = "4")]
    eg_rout: u32,
    #[prost(uint32, tag = "5")]
    eg_out: u32,
    #[prost(uint32, tag = "6")]
    eg_gen: u32,
    #[prost(uint32, tag = "7")]
    eg_ksl: u32,
    #[prost(uint32, tag = "8")]
    eg_tl_ksl: u32,
    #[prost(uint32, tag = "9")]
    eg_ks: u32,
    #[prost(bytes = "vec", tag = "10")]
    eg_rate_hi: Vec<u8>,
    #[prost(bytes = "vec", tag = "11")]
    eg_rate_lo: Vec<u8>,
    #[prost(uint32, tag = "12")]
    eg_rate_nonzero_mask: u32,
    #[prost(bool, tag = "13")]
    trem_enabled: bool,
    #[prost(uint32, tag = "14")]
    reg_vib: u32,
    #[prost(uint32, tag = "15")]
    reg_type: u32,
    #[prost(uint32, tag = "16")]
    reg_ksr: u32,
    #[prost(uint32, tag = "17")]
    reg_mult: u32,
    #[prost(uint32, tag = "18")]
    reg_ksl: u32,
    #[prost(uint32, tag = "19")]
    reg_tl: u32,
    #[prost(uint32, tag = "20")]
    reg_ar: u32,
    #[prost(uint32, tag = "21")]
    reg_dr: u32,
    #[prost(uint32, tag = "22")]
    reg_sl: u32,
    #[prost(uint32, tag = "23")]
    reg_rr: u32,
    #[prost(uint32, tag = "24")]
    reg_wf: u32,
    #[prost(uint32, tag = "25")]
    key: u32,
    #[prost(uint32, tag = "26")]
    pg_reset: u32,
    #[prost(uint32, tag = "27")]
    pg_phase: u32,
    #[prost(uint32, tag = "28")]
    pg_inc: u32,
    #[prost(uint32, repeated, tag = "29")]
    pg_inc_vib: Vec<u32>,
    #[prost(uint32, tag = "30")]
    pg_phase_out: u32,
    #[prost(uint64, tag = "31")]
    slot_num: u64,
}
impl State for Slot {
    type Wire = OplCoreSlot;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            channel: *(&self.channel) as u64,
            mod_idx: *(&self.mod_idx) as u64,
            prout: i32::from(*(&self.prout)),
            eg_rout: u32::from(*(&self.eg_rout)),
            eg_out: u32::from(*(&self.eg_out)),
            eg_gen: u32::from(*(&self.eg_gen)),
            eg_ksl: u32::from(*(&self.eg_ksl)),
            eg_tl_ksl: u32::from(*(&self.eg_tl_ksl)),
            eg_ks: u32::from(*(&self.eg_ks)),
            eg_rate_hi: (&self.eg_rate_hi).to_vec(),
            eg_rate_lo: (&self.eg_rate_lo).to_vec(),
            eg_rate_nonzero_mask: u32::from(*(&self.eg_rate_nonzero_mask)),
            trem_enabled: *(&self.trem_enabled),
            reg_vib: u32::from(*(&self.reg_vib)),
            reg_type: u32::from(*(&self.reg_type)),
            reg_ksr: u32::from(*(&self.reg_ksr)),
            reg_mult: u32::from(*(&self.reg_mult)),
            reg_ksl: u32::from(*(&self.reg_ksl)),
            reg_tl: u32::from(*(&self.reg_tl)),
            reg_ar: u32::from(*(&self.reg_ar)),
            reg_dr: u32::from(*(&self.reg_dr)),
            reg_sl: u32::from(*(&self.reg_sl)),
            reg_rr: u32::from(*(&self.reg_rr)),
            reg_wf: u32::from(*(&self.reg_wf)),
            key: u32::from(*(&self.key)),
            pg_reset: *(&self.pg_reset),
            pg_phase: *(&self.pg_phase),
            pg_inc: *(&self.pg_inc),
            pg_inc_vib: (&self.pg_inc_vib).iter().map(|v| *(v)).collect(),
            pg_phase_out: u32::from(*(&self.pg_phase_out)),
            slot_num: *(&self.slot_num) as u64,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            channel: doslet_state::narrow(w.channel, "OplCoreSlot.channel")?,
            mod_idx: doslet_state::narrow(w.mod_idx, "OplCoreSlot.mod_idx")?,
            prout: doslet_state::narrow(w.prout, "OplCoreSlot.prout")?,
            eg_rout: doslet_state::narrow(w.eg_rout, "OplCoreSlot.eg_rout")?,
            eg_out: doslet_state::narrow(w.eg_out, "OplCoreSlot.eg_out")?,
            eg_gen: doslet_state::narrow(w.eg_gen, "OplCoreSlot.eg_gen")?,
            eg_ksl: doslet_state::narrow(w.eg_ksl, "OplCoreSlot.eg_ksl")?,
            eg_tl_ksl: doslet_state::narrow(w.eg_tl_ksl, "OplCoreSlot.eg_tl_ksl")?,
            eg_ks: doslet_state::narrow(w.eg_ks, "OplCoreSlot.eg_ks")?,
            eg_rate_hi: doslet_state::narrow(w.eg_rate_hi, "OplCoreSlot.eg_rate_hi length")?,
            eg_rate_lo: doslet_state::narrow(w.eg_rate_lo, "OplCoreSlot.eg_rate_lo length")?,
            eg_rate_nonzero_mask: doslet_state::narrow(
                w.eg_rate_nonzero_mask,
                "OplCoreSlot.eg_rate_nonzero_mask",
            )?,
            trem_enabled: w.trem_enabled,
            reg_vib: doslet_state::narrow(w.reg_vib, "OplCoreSlot.reg_vib")?,
            reg_type: doslet_state::narrow(w.reg_type, "OplCoreSlot.reg_type")?,
            reg_ksr: doslet_state::narrow(w.reg_ksr, "OplCoreSlot.reg_ksr")?,
            reg_mult: doslet_state::narrow(w.reg_mult, "OplCoreSlot.reg_mult")?,
            reg_ksl: doslet_state::narrow(w.reg_ksl, "OplCoreSlot.reg_ksl")?,
            reg_tl: doslet_state::narrow(w.reg_tl, "OplCoreSlot.reg_tl")?,
            reg_ar: doslet_state::narrow(w.reg_ar, "OplCoreSlot.reg_ar")?,
            reg_dr: doslet_state::narrow(w.reg_dr, "OplCoreSlot.reg_dr")?,
            reg_sl: doslet_state::narrow(w.reg_sl, "OplCoreSlot.reg_sl")?,
            reg_rr: doslet_state::narrow(w.reg_rr, "OplCoreSlot.reg_rr")?,
            reg_wf: doslet_state::narrow(w.reg_wf, "OplCoreSlot.reg_wf")?,
            key: doslet_state::narrow(w.key, "OplCoreSlot.key")?,
            pg_reset: w.pg_reset,
            pg_phase: w.pg_phase,
            pg_inc: w.pg_inc,
            pg_inc_vib: doslet_state::narrow(
                w.pg_inc_vib
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreSlot.pg_inc_vib length",
            )?,
            pg_phase_out: doslet_state::narrow(w.pg_phase_out, "OplCoreSlot.pg_phase_out")?,
            slot_num: doslet_state::narrow(w.slot_num, "OplCoreSlot.slot_num")?,
        };
        if !(value.channel < 18
            && value.mod_idx < 74
            && value.eg_gen < 4
            && value.eg_rate_lo.iter().all(|&r| r < 4)
            && value.reg_wf < 8
            && value.reg_mult < 16
            && value.reg_ksl < 4
            && value.slot_num < 36)
        {
            return Err(InvalidState("OplCoreSlot invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct OplCoreChannel {
    #[prost(uint64, repeated, tag = "1")]
    slotz: Vec<u64>,
    #[prost(uint64, optional, tag = "2")]
    pair: Option<u64>,
    #[prost(uint64, repeated, tag = "3")]
    out: Vec<u64>,
    #[prost(uint32, tag = "4")]
    chtype: u32,
    #[prost(uint32, tag = "5")]
    f_num: u32,
    #[prost(uint32, tag = "6")]
    block: u32,
    #[prost(uint32, tag = "7")]
    fb: u32,
    #[prost(uint32, tag = "8")]
    con: u32,
    #[prost(uint32, tag = "9")]
    alg: u32,
    #[prost(uint32, tag = "10")]
    ksv: u32,
    #[prost(uint32, tag = "11")]
    cha: u32,
    #[prost(uint32, tag = "12")]
    chb: u32,
    #[prost(uint32, tag = "13")]
    chc: u32,
    #[prost(uint32, tag = "14")]
    chd: u32,
    #[prost(uint32, tag = "15")]
    mix_mask: u32,
    #[prost(sint32, tag = "16")]
    leftpan: i32,
    #[prost(sint32, tag = "17")]
    rightpan: i32,
    #[prost(uint64, tag = "18")]
    ch_num: u64,
}
impl State for Channel {
    type Wire = OplCoreChannel;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            slotz: (&self.slotz).iter().map(|v| *(v) as u64).collect(),
            pair: (&self.pair).as_ref().map(|v| *(v) as u64),
            out: (&self.out).iter().map(|v| *(v) as u64).collect(),
            chtype: u32::from(*(&self.chtype)),
            f_num: u32::from(*(&self.f_num)),
            block: u32::from(*(&self.block)),
            fb: u32::from(*(&self.fb)),
            con: u32::from(*(&self.con)),
            alg: u32::from(*(&self.alg)),
            ksv: u32::from(*(&self.ksv)),
            cha: u32::from(*(&self.cha)),
            chb: u32::from(*(&self.chb)),
            chc: u32::from(*(&self.chc)),
            chd: u32::from(*(&self.chd)),
            mix_mask: u32::from(*(&self.mix_mask)),
            leftpan: *(&self.leftpan),
            rightpan: *(&self.rightpan),
            ch_num: *(&self.ch_num) as u64,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            slotz: doslet_state::narrow(
                w.slotz
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        Ok(doslet_state::narrow(v, "OplCoreChannel.slotz")?)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChannel.slotz length",
            )?,
            pair: w
                .pair
                .map(|v| -> Result<_, InvalidState> {
                    Ok(doslet_state::narrow(v, "OplCoreChannel.pair")?)
                })
                .transpose()?,
            out: doslet_state::narrow(
                w.out
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        Ok(doslet_state::narrow(v, "OplCoreChannel.out")?)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChannel.out length",
            )?,
            chtype: doslet_state::narrow(w.chtype, "OplCoreChannel.chtype")?,
            f_num: doslet_state::narrow(w.f_num, "OplCoreChannel.f_num")?,
            block: doslet_state::narrow(w.block, "OplCoreChannel.block")?,
            fb: doslet_state::narrow(w.fb, "OplCoreChannel.fb")?,
            con: doslet_state::narrow(w.con, "OplCoreChannel.con")?,
            alg: doslet_state::narrow(w.alg, "OplCoreChannel.alg")?,
            ksv: doslet_state::narrow(w.ksv, "OplCoreChannel.ksv")?,
            cha: doslet_state::narrow(w.cha, "OplCoreChannel.cha")?,
            chb: doslet_state::narrow(w.chb, "OplCoreChannel.chb")?,
            chc: doslet_state::narrow(w.chc, "OplCoreChannel.chc")?,
            chd: doslet_state::narrow(w.chd, "OplCoreChannel.chd")?,
            mix_mask: doslet_state::narrow(w.mix_mask, "OplCoreChannel.mix_mask")?,
            leftpan: w.leftpan,
            rightpan: w.rightpan,
            ch_num: doslet_state::narrow(w.ch_num, "OplCoreChannel.ch_num")?,
        };
        if !(value.slotz.iter().all(|&s| s < 36)
            && value.pair.is_none_or(|p| p < 18)
            && value.out.iter().all(|&o| o < 74)
            && value.ch_num < 18
            && value.block < 8
            && value.fb < 8)
        {
            return Err(InvalidState("OplCoreChannel invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct OplCoreWriteBuf {
    #[prost(uint64, tag = "1")]
    time: u64,
    #[prost(uint32, tag = "2")]
    reg: u32,
    #[prost(uint32, tag = "3")]
    data: u32,
}
impl State for WriteBuf {
    type Wire = OplCoreWriteBuf;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            time: *(&self.time),
            reg: u32::from(*(&self.reg)),
            data: u32::from(*(&self.data)),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            time: w.time,
            reg: doslet_state::narrow(w.reg, "OplCoreWriteBuf.reg")?,
            data: doslet_state::narrow(w.data, "OplCoreWriteBuf.data")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct OplCoreChip {
    #[prost(message, repeated, tag = "1")]
    channels: Vec<<Channel as State>::Wire>,
    #[prost(message, repeated, tag = "2")]
    slots: Vec<<Slot as State>::Wire>,
    #[prost(sint32, repeated, tag = "3")]
    outputs: Vec<i32>,
    #[prost(uint32, tag = "4")]
    timer: u32,
    #[prost(uint64, tag = "5")]
    eg_timer: u64,
    #[prost(uint32, tag = "6")]
    eg_timerrem: u32,
    #[prost(uint32, tag = "7")]
    eg_state: u32,
    #[prost(uint32, tag = "8")]
    eg_add: u32,
    #[prost(uint32, tag = "9")]
    eg_timer_lo: u32,
    #[prost(uint32, tag = "10")]
    newm: u32,
    #[prost(uint32, tag = "11")]
    nts: u32,
    #[prost(uint32, tag = "12")]
    rhy: u32,
    #[prost(uint32, tag = "13")]
    vibpos: u32,
    #[prost(uint32, tag = "14")]
    vibshift: u32,
    #[prost(uint32, tag = "15")]
    tremolo: u32,
    #[prost(uint32, tag = "16")]
    tremolopos: u32,
    #[prost(uint32, tag = "17")]
    tremoloshift: u32,
    #[prost(uint32, tag = "18")]
    noise: u32,
    #[prost(uint32, tag = "19")]
    noise_hh: u32,
    #[prost(uint32, tag = "20")]
    noise_sd: u32,
    #[prost(sint32, repeated, tag = "21")]
    mixbuff: Vec<i32>,
    #[prost(uint32, tag = "22")]
    rm_hh_bit2: u32,
    #[prost(uint32, tag = "23")]
    rm_hh_bit3: u32,
    #[prost(uint32, tag = "24")]
    rm_hh_bit7: u32,
    #[prost(uint32, tag = "25")]
    rm_hh_bit8: u32,
    #[prost(uint32, tag = "26")]
    rm_tc_bit3: u32,
    #[prost(uint32, tag = "27")]
    rm_tc_bit5: u32,
    #[prost(uint32, tag = "28")]
    stereoext: u32,
    #[prost(sint32, tag = "29")]
    rateratio: i32,
    #[prost(sint32, tag = "30")]
    samplecnt: i32,
    #[prost(sint32, repeated, tag = "31")]
    oldsamples: Vec<i32>,
    #[prost(sint32, repeated, tag = "32")]
    samples: Vec<i32>,
    #[prost(uint64, tag = "33")]
    writebuf_samplecnt: u64,
    #[prost(uint64, tag = "34")]
    writebuf_cur: u64,
    #[prost(uint64, tag = "35")]
    writebuf_last: u64,
    #[prost(uint64, tag = "36")]
    writebuf_lasttime: u64,
    #[prost(message, repeated, tag = "37")]
    writebuf: Vec<<WriteBuf as State>::Wire>,
}
impl State for Chip {
    type Wire = OplCoreChip;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            channels: (&self.channels).iter().map(|v| State::save(v)).collect(),
            slots: (&self.slots).iter().map(|v| State::save(v)).collect(),
            outputs: (&self.outputs).iter().map(|v| i32::from(*(v))).collect(),
            timer: u32::from(*(&self.timer)),
            eg_timer: *(&self.eg_timer),
            eg_timerrem: u32::from(*(&self.eg_timerrem)),
            eg_state: u32::from(*(&self.eg_state)),
            eg_add: u32::from(*(&self.eg_add)),
            eg_timer_lo: u32::from(*(&self.eg_timer_lo)),
            newm: u32::from(*(&self.newm)),
            nts: u32::from(*(&self.nts)),
            rhy: u32::from(*(&self.rhy)),
            vibpos: u32::from(*(&self.vibpos)),
            vibshift: u32::from(*(&self.vibshift)),
            tremolo: u32::from(*(&self.tremolo)),
            tremolopos: u32::from(*(&self.tremolopos)),
            tremoloshift: u32::from(*(&self.tremoloshift)),
            noise: *(&self.noise),
            noise_hh: u32::from(*(&self.noise_hh)),
            noise_sd: u32::from(*(&self.noise_sd)),
            mixbuff: (&self.mixbuff).iter().map(|v| *(v)).collect(),
            rm_hh_bit2: u32::from(*(&self.rm_hh_bit2)),
            rm_hh_bit3: u32::from(*(&self.rm_hh_bit3)),
            rm_hh_bit7: u32::from(*(&self.rm_hh_bit7)),
            rm_hh_bit8: u32::from(*(&self.rm_hh_bit8)),
            rm_tc_bit3: u32::from(*(&self.rm_tc_bit3)),
            rm_tc_bit5: u32::from(*(&self.rm_tc_bit5)),
            stereoext: u32::from(*(&self.stereoext)),
            rateratio: *(&self.rateratio),
            samplecnt: *(&self.samplecnt),
            oldsamples: (&self.oldsamples).iter().map(|v| i32::from(*(v))).collect(),
            samples: (&self.samples).iter().map(|v| i32::from(*(v))).collect(),
            writebuf_samplecnt: *(&self.writebuf_samplecnt),
            writebuf_cur: *(&self.writebuf_cur) as u64,
            writebuf_last: *(&self.writebuf_last) as u64,
            writebuf_lasttime: *(&self.writebuf_lasttime),
            writebuf: (&self.writebuf).iter().map(|v| State::save(v)).collect(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            channels: doslet_state::narrow(
                w.channels
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(<Channel as State>::load(v)?) })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChip.channels length",
            )?,
            slots: doslet_state::narrow(
                w.slots
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(<Slot as State>::load(v)?) })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChip.slots length",
            )?,
            outputs: doslet_state::narrow(
                w.outputs
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        Ok(doslet_state::narrow(v, "OplCoreChip.outputs")?)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChip.outputs length",
            )?,
            timer: doslet_state::narrow(w.timer, "OplCoreChip.timer")?,
            eg_timer: w.eg_timer,
            eg_timerrem: doslet_state::narrow(w.eg_timerrem, "OplCoreChip.eg_timerrem")?,
            eg_state: doslet_state::narrow(w.eg_state, "OplCoreChip.eg_state")?,
            eg_add: doslet_state::narrow(w.eg_add, "OplCoreChip.eg_add")?,
            eg_timer_lo: doslet_state::narrow(w.eg_timer_lo, "OplCoreChip.eg_timer_lo")?,
            newm: doslet_state::narrow(w.newm, "OplCoreChip.newm")?,
            nts: doslet_state::narrow(w.nts, "OplCoreChip.nts")?,
            rhy: doslet_state::narrow(w.rhy, "OplCoreChip.rhy")?,
            vibpos: doslet_state::narrow(w.vibpos, "OplCoreChip.vibpos")?,
            vibshift: doslet_state::narrow(w.vibshift, "OplCoreChip.vibshift")?,
            tremolo: doslet_state::narrow(w.tremolo, "OplCoreChip.tremolo")?,
            tremolopos: doslet_state::narrow(w.tremolopos, "OplCoreChip.tremolopos")?,
            tremoloshift: doslet_state::narrow(w.tremoloshift, "OplCoreChip.tremoloshift")?,
            noise: w.noise,
            noise_hh: doslet_state::narrow(w.noise_hh, "OplCoreChip.noise_hh")?,
            noise_sd: doslet_state::narrow(w.noise_sd, "OplCoreChip.noise_sd")?,
            mixbuff: doslet_state::narrow(
                w.mixbuff
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChip.mixbuff length",
            )?,
            rm_hh_bit2: doslet_state::narrow(w.rm_hh_bit2, "OplCoreChip.rm_hh_bit2")?,
            rm_hh_bit3: doslet_state::narrow(w.rm_hh_bit3, "OplCoreChip.rm_hh_bit3")?,
            rm_hh_bit7: doslet_state::narrow(w.rm_hh_bit7, "OplCoreChip.rm_hh_bit7")?,
            rm_hh_bit8: doslet_state::narrow(w.rm_hh_bit8, "OplCoreChip.rm_hh_bit8")?,
            rm_tc_bit3: doslet_state::narrow(w.rm_tc_bit3, "OplCoreChip.rm_tc_bit3")?,
            rm_tc_bit5: doslet_state::narrow(w.rm_tc_bit5, "OplCoreChip.rm_tc_bit5")?,
            stereoext: doslet_state::narrow(w.stereoext, "OplCoreChip.stereoext")?,
            rateratio: w.rateratio,
            samplecnt: w.samplecnt,
            oldsamples: doslet_state::narrow(
                w.oldsamples
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        Ok(doslet_state::narrow(v, "OplCoreChip.oldsamples")?)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChip.oldsamples length",
            )?,
            samples: doslet_state::narrow(
                w.samples
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        Ok(doslet_state::narrow(v, "OplCoreChip.samples")?)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChip.samples length",
            )?,
            writebuf_samplecnt: w.writebuf_samplecnt,
            writebuf_cur: doslet_state::narrow(w.writebuf_cur, "OplCoreChip.writebuf_cur")?,
            writebuf_last: doslet_state::narrow(w.writebuf_last, "OplCoreChip.writebuf_last")?,
            writebuf_lasttime: w.writebuf_lasttime,
            writebuf: doslet_state::narrow(
                w.writebuf
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(<WriteBuf as State>::load(v)?) })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplCoreChip.writebuf length",
            )?,
        };
        if !(value.rateratio > 0
            && value.eg_timer_lo < 4
            && value.vibpos < 8
            && value.vibshift < 8
            && value.tremoloshift < 8
            && value.writebuf_cur < WRITEBUF_SIZE
            && value.writebuf_last < WRITEBUF_SIZE)
        {
            return Err(InvalidState("OplCoreChip invariants"));
        }
        Ok(value)
    }
}
