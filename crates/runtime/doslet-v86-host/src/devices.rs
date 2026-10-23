//! Synchronous, scoped device callbacks. The CPU owns RAM; a callback borrows
//! that same RAM only while WASM execution is suspended in the host call.
use doslet_bus::BusError;
use doslet_v86::DeviceRequest;

pub trait DeviceHost {
    fn access(&mut self, ram: &mut [u8], request: DeviceRequest) -> Result<u32, BusError>;
}

// Only this module erases the lifetime of a synchronous device borrow. The
// parent installs it before entering WASM and clears it before returning or
// unwinding. Neither the pointer nor a RAM borrow is exposed to guest code.
#[allow(unsafe_code)]
mod scoped {
    use super::*;
    pub(crate) struct Borrow {
        pointer: *mut (),
        access: unsafe fn(*mut (), &mut [u8], DeviceRequest) -> Result<u32, BusError>,
    }
    // A live borrow exists only inside an exclusive synchronous &mut Cpu call;
    // that owner cannot move to another thread until the borrow is cleared.
    unsafe impl Send for Borrow {}
    impl Borrow {
        pub(crate) fn new<H: DeviceHost>(host: &mut H) -> Self {
            unsafe fn access<H: DeviceHost>(
                pointer: *mut (),
                ram: &mut [u8],
                request: DeviceRequest,
            ) -> Result<u32, BusError> {
                // SAFETY: the installer retains the exclusive H borrow for the
                // complete call and removes this thunk before it expires.
                unsafe { &mut *pointer.cast::<H>() }.access(ram, request)
            }
            Self {
                pointer: core::ptr::from_mut(host).cast(),
                access: access::<H>,
            }
        }
        pub(crate) fn access(
            &mut self,
            ram: &mut [u8],
            request: DeviceRequest,
        ) -> Result<u32, BusError> {
            // SAFETY: no thunk can be invoked outside the installer's scope.
            unsafe { (self.access)(self.pointer, ram, request) }
        }
    }
}
pub(crate) use scoped::Borrow;
