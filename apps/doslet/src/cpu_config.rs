//! Presentation drafts; execution changes are applied by the machine owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Fixed,
    Tiers,
    Adaptive,
}
impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Fixed => "Fixed speed",
            Self::Tiers => "Hot / Grey / Cold",
            Self::Adaptive => "Adaptive · experimental",
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Core {
    Interpreter,
    Jit,
    /// Installed procedure module; the interpreter runs everything else.
    Aot,
}
impl std::fmt::Display for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Interpreter => "Interpreter",
            Self::Jit => "JIT · Cranelift",
            Self::Aot => "AOT · Wasmtime",
        })
    }
}
pub const JIT_AVAILABLE: bool = cfg!(all(
    feature = "dynamic-core",
    any(target_arch = "x86_64", target_arch = "aarch64")
));
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub mode: Mode,
    pub core: Core,
    pub mhz: u16,
    pub grey: u16,
    pub hot: Option<u16>,
    pub cold: u16,
    pub learning: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Fixed,
            core: Core::Interpreter,
            mhz: 133,
            grey: 133,
            hot: Some(500),
            cold: 8,
            learning: false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Edit {
    Mode(Mode),
    Core(Core),
    Mhz(u16),
    Grey(u16),
    Hot(u16),
    Cold(u16),
    Uncapped(bool),
    Learning(bool),
    Apply,
    Revert,
}
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub enum ProfileEdit {
    New,
    Open,
    Path(String),
    Submit,
    Cancel,
}
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct ProfileForm {
    pub path: String,
    pub open: bool,
    pub error: Option<String>,
}
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct ProfileInfo {
    pub path: Option<String>,
    pub labels: usize,
}
impl Settings {
    pub fn edit(&mut self, edit: Edit) {
        let rate = |v: u16| v.clamp(1, 1000);
        match edit {
            Edit::Mode(v) => {
                self.mode = v;
                if v != Mode::Tiers {
                    self.learning = false;
                }
            }
            Edit::Core(v) => self.core = v,
            Edit::Mhz(v) => self.mhz = rate(v),
            Edit::Grey(v) => self.grey = rate(v),
            Edit::Hot(v) => self.hot = Some(rate(v)),
            Edit::Cold(v) => self.cold = rate(v),
            Edit::Uncapped(v) => self.hot = if v { None } else { Some(500.max(self.grey)) },
            Edit::Learning(v) => self.learning = v,
            Edit::Apply | Edit::Revert => {}
        }
    }
    pub fn valid(self) -> bool {
        [Some(self.mhz), Some(self.grey), Some(self.cold), self.hot]
            .into_iter()
            .flatten()
            .all(|v| (1..=1000).contains(&v))
            && (self.core != Core::Jit || JIT_AVAILABLE)
    }
}
