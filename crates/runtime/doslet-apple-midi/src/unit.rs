//! Private FFI: one owned Audio Unit, used synchronously on its owner's thread.
use coreaudio_sys::*;
use std::{ffi::c_void, mem, ptr};

pub(super) const MAX_FRAMES: usize = 512;

// AudioBufferList ends with a variable-length array. This layout reserves two
// entries, with the same C alignment and prefix as the SDK's one-entry struct.
#[repr(C)]
struct StereoBuffers {
    count: u32,
    buffers: [AudioBuffer; 2],
}

pub(super) struct Unit {
    instance: AudioUnit,
    initialized: bool,
    frames: u64,
    left: [f32; MAX_FRAMES],
    right: [f32; MAX_FRAMES],
}

fn check(status: OSStatus, operation: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("Apple MIDI {operation}: OSStatus {status}"))
    }
}

impl Unit {
    pub(super) fn new(rate: u32) -> Result<Self, String> {
        let description = AudioComponentDescription {
            componentType: kAudioUnitType_MusicDevice,
            componentSubType: kAudioUnitSubType_DLSSynth,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        // SAFETY: the description is initialized and borrowed for this call.
        let component = unsafe { AudioComponentFindNext(ptr::null_mut(), &description) };
        if component.is_null() {
            return Err("Apple DLSMusicDevice is unavailable".into());
        }
        let mut instance = ptr::null_mut();
        // SAFETY: component came from Core Audio; instance is a valid out pointer.
        check(
            unsafe { AudioComponentInstanceNew(component, &mut instance) },
            "create",
        )?;
        if instance.is_null() {
            return Err("Apple DLSMusicDevice returned a null instance".into());
        }
        let mut unit = Self {
            instance,
            initialized: false,
            frames: 0,
            left: [0.0; MAX_FRAMES],
            right: [0.0; MAX_FRAMES],
        };
        let format = AudioStreamBasicDescription {
            mSampleRate: f64::from(rate),
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagsNativeFloatPacked | kAudioFormatFlagIsNonInterleaved,
            mBytesPerPacket: 4,
            mFramesPerPacket: 1,
            mBytesPerFrame: 4,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        unit.property(
            kAudioUnitProperty_StreamFormat,
            kAudioUnitScope_Output,
            &format,
        )?;
        unit.property(
            kAudioUnitProperty_MaximumFramesPerSlice,
            kAudioUnitScope_Global,
            &(MAX_FRAMES as u32),
        )?;
        // SAFETY: this instance is owned and fully configured. No callbacks or
        // concurrent operations are registered. Drop cleans up on every error.
        check(
            unsafe { AudioUnitInitialize(unit.instance) },
            "initialize system bank",
        )?;
        unit.initialized = true;
        Ok(unit)
    }

    fn property<T>(
        &mut self,
        property: AudioUnitPropertyID,
        scope: AudioUnitScope,
        value: &T,
    ) -> Result<(), String> {
        // SAFETY: private callers pass the SDK type required by each property;
        // Core Audio copies the value during the call, retaining no reference.
        check(
            unsafe {
                AudioUnitSetProperty(
                    self.instance,
                    property,
                    scope,
                    0,
                    ptr::from_ref(value).cast::<c_void>(),
                    mem::size_of::<T>() as u32,
                )
            },
            "set format/limits",
        )
    }

    pub(super) fn message(&mut self, message: &[u8]) -> Result<(), String> {
        // Only the bounded MIDI decoder calls this method with complete messages.
        match message[0] {
            0x80..=0xEF => {
                // SAFETY: the instance is live; MIDI bytes are scalar values.
                check(
                    unsafe {
                        MusicDeviceMIDIEvent(
                            self.instance,
                            u32::from(message[0]),
                            u32::from(message[1]),
                            u32::from(message.get(2).copied().unwrap_or(0)),
                            0,
                        )
                    },
                    "MIDI event",
                )
            }
            0xF0 => self.sysex(message),
            0xFF => self.sysex(&[0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7]),
            // Transport/clock/song-position messages do not affect GM synthesis.
            _ => Ok(()),
        }
    }

    fn sysex(&mut self, message: &[u8]) -> Result<(), String> {
        // SAFETY: the bounded, complete message remains valid throughout the
        // call. MusicDeviceSysEx consumes/copies the supplied bytes synchronously.
        check(
            unsafe { MusicDeviceSysEx(self.instance, message.as_ptr(), message.len() as u32) },
            "SysEx",
        )
    }

    pub(super) fn render(&mut self, output: &mut [[f32; 2]]) -> Result<(), String> {
        assert!(!output.is_empty() && output.len() <= MAX_FRAMES);
        let bytes = mem::size_of_val(output) as u32 / 2;
        let mut buffers = StereoBuffers {
            count: 2,
            buffers: [
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: self.left.as_mut_ptr().cast(),
                },
                AudioBuffer {
                    mNumberChannels: 1,
                    mDataByteSize: bytes,
                    mData: self.right.as_mut_ptr().cast(),
                },
            ],
        };
        // SAFETY: all-zero is valid for the C timestamp's numeric fields; only
        // sample time is marked valid, independent of wall time or device time.
        let mut time: AudioTimeStamp = unsafe { mem::zeroed() };
        time.mSampleTime = self.frames as f64;
        time.mFlags = kAudioTimeStampSampleTimeValid;
        let mut flags = 0;
        // SAFETY: the two-channel list and its non-overlapping f32 buffers are
        // valid for the bounded frame count. The instance never outlives them
        // through a callback; rendering is synchronous, with exclusive access.
        check(
            unsafe {
                AudioUnitRender(
                    self.instance,
                    &mut flags,
                    &time,
                    0,
                    output.len() as u32,
                    ptr::from_mut(&mut buffers).cast::<AudioBufferList>(),
                )
            },
            "render",
        )?;
        if flags & kAudioUnitRenderAction_OutputIsSilence != 0 {
            output.fill([0.0; 2]);
        } else {
            for (index, frame) in output.iter_mut().enumerate() {
                *frame = [self.left[index], self.right[index]];
            }
        }
        self.frames += output.len() as u64;
        Ok(())
    }
}

impl Drop for Unit {
    fn drop(&mut self) {
        // SAFETY: unique live instance created above; there are no callbacks,
        // borrowed buffers or concurrent users when ownership is released.
        unsafe {
            if self.initialized {
                AudioUnitUninitialize(self.instance);
            }
            AudioComponentInstanceDispose(self.instance);
        }
    }
}
