use super::*;
use doslet_x86_wasm::static_program::{
    Accounting, Address, Alu, CompileOptions, Condition, Operation, Program, Region, RegionId,
    Register, Source, Terminator,
};

#[derive(Default)]
struct Services {
    calls: usize,
    change_entry: bool,
    error: bool,
}
impl Runtime for Services {
    type Error = &'static str;
    fn callback(&mut self, frame: &mut Frame, reason: Reason) -> Result<Control, Self::Error> {
        self.calls += 1;
        if self.error {
            return Err("test service failure");
        }
        if self.change_entry {
            assert_eq!(reason, Reason::Service);
            frame.address.offset = 10;
            frame.gpr[0] = 100;
            self.change_entry = false;
            return Ok(Control::Continue);
        }
        Ok(if reason == Reason::Fallback {
            Control::Exit
        } else {
            Control::Continue
        })
    }
}
fn address(offset: u32) -> Address {
    Address { space: 1, offset }
}
fn program(accounting: Accounting) -> Program {
    Program {
        regions: vec![
            Region {
                id: RegionId(0),
                entry: address(0),
                operations: vec![
                    Operation::Inc(Register::Al),
                    Operation::Alu {
                        kind: Alu::Cmp,
                        target: Register::Al,
                        source: Source::Immediate(5),
                    },
                ],
                terminator: Terminator::Branch {
                    condition: Condition::NotEqual,
                    taken: RegionId(0),
                    not_taken: RegionId(1),
                },
                accounting,
            },
            Region {
                id: RegionId(1),
                entry: address(10),
                operations: vec![],
                terminator: Terminator::Fallback(address(10)),
                accounting: Accounting::Counted,
            },
        ],
    }
}
fn artifact(program: &Program, interval: u32) -> Artifact {
    let compiled = program
        .compile(&CompileOptions {
            service_interval_regions: interval,
            ..CompileOptions::default()
        })
        .unwrap();
    Artifact::compile(compiled.wasm()).unwrap()
}

#[test]
fn native_static_tail_calls_soft_budget_and_runtime_exit() {
    let artifact = artifact(&program(Accounting::Counted), 64);
    let mut runner = artifact.instantiate_static(Services::default()).unwrap();
    let mut frame = Frame {
        address: address(0),
        remaining_budget: 4,
        ..Frame::default()
    };
    assert_eq!(runner.run(&mut frame).unwrap(), Exit::Budget);
    assert_eq!(
        (frame.gpr[0], frame.charged_work, frame.remaining_budget),
        (2, 6, 0)
    );
    assert_eq!(runner.runtime().calls, 0);
    frame.remaining_budget = 100;
    assert_eq!(runner.run(&mut frame).unwrap(), Exit::Runtime);
    assert_eq!(
        (frame.gpr[0], frame.charged_work, frame.address),
        (5, 15, address(10))
    );
    assert_eq!(runner.runtime().calls, 1);
}

#[test]
fn native_static_callback_changes_live_registers_and_entry() {
    let artifact = artifact(&program(Accounting::Counted), 1);
    let mut runner = artifact
        .instantiate_static(Services {
            change_entry: true,
            ..Services::default()
        })
        .unwrap();
    let mut frame = Frame {
        address: address(0),
        remaining_budget: 100,
        ..Frame::default()
    };
    assert_eq!(runner.run(&mut frame).unwrap(), Exit::Runtime);
    assert_eq!(frame.gpr[0], 100);
    assert_eq!(frame.charged_work, 3);
    assert_eq!(runner.runtime().calls, 2);
}

#[test]
fn native_hot_does_not_charge_and_still_services() {
    let artifact = artifact(&program(Accounting::UncountedHot), 2);
    let mut runner = artifact.instantiate_static(Services::default()).unwrap();
    let mut frame = Frame {
        address: address(0),
        remaining_budget: 100,
        charged_work: 9,
        ..Frame::default()
    };
    assert_eq!(runner.run(&mut frame).unwrap(), Exit::Runtime);
    assert_eq!(frame.gpr[0], 5);
    assert_eq!(frame.charged_work, 9);
    assert_eq!(frame.remaining_budget, 100);
    assert_eq!(frame.exact_total(), None);
    assert_eq!(runner.runtime().calls, 3);
}

#[test]
fn runtime_error_stops_without_replaying_and_instances_are_independent() {
    let artifact = artifact(&program(Accounting::Counted), 1);
    let mut first = artifact
        .instantiate_static(Services {
            error: true,
            ..Services::default()
        })
        .unwrap();
    let mut second = artifact.instantiate_static(Services::default()).unwrap();
    let mut frame = Frame {
        address: address(0),
        remaining_budget: 100,
        ..Frame::default()
    };
    assert!(
        first
            .run(&mut frame)
            .unwrap_err()
            .contains("test service failure")
    );
    assert!(first.run(&mut frame).unwrap_err().contains("stopped"));
    assert_eq!(first.runtime().calls, 1);
    assert_eq!(second.runtime().calls, 0);
    frame = Frame {
        address: address(0),
        remaining_budget: 100,
        ..Frame::default()
    };
    assert_eq!(second.run(&mut frame).unwrap(), Exit::Runtime);
    assert_eq!(frame.gpr[0], 5);
}

#[derive(Default)]
struct AdmissionServices {
    horizon: u32,
    admissions: Vec<(u32, u32)>,
    frames: Vec<Frame>,
    fail: bool,
}
impl Runtime for AdmissionServices {
    type Error = &'static str;

    fn callback(&mut self, frame: &mut Frame, reason: Reason) -> Result<Control, Self::Error> {
        assert_eq!(reason, Reason::Fallback);
        self.frames.push(*frame);
        Ok(Control::Exit)
    }

    fn admit(&mut self, id: u32, required: u32) -> Result<bool, Self::Error> {
        self.admissions.push((id, required));
        if self.fail {
            return Err("test admission failure");
        }
        if required > self.horizon {
            return Ok(false);
        }
        self.horizon -= required;
        Ok(true)
    }
}

fn admitted_artifact() -> Artifact {
    let compiled = program(Accounting::Counted)
        .compile(&CompileOptions {
            runtime_admission: true,
            ..CompileOptions::default()
        })
        .unwrap();
    Artifact::compile(compiled.wasm()).unwrap()
}

#[test]
fn native_admission_reserves_a_hard_horizon_and_default_keeps_soft_budget() {
    let artifact = admitted_artifact();
    let mut runner = artifact
        .instantiate_static(AdmissionServices {
            horizon: 5,
            ..AdmissionServices::default()
        })
        .unwrap();
    let mut frame = Frame {
        address: address(0),
        remaining_budget: 100,
        ..Frame::default()
    };
    assert_eq!(runner.run(&mut frame).unwrap(), Exit::Runtime);
    assert_eq!(
        (frame.gpr[0], frame.charged_work, frame.remaining_budget),
        (1, 3, 97)
    );
    assert_eq!(frame.address, address(0));
    assert_eq!(runner.runtime().horizon, 2);
    assert_eq!(runner.runtime().admissions, [(0, 3), (0, 3)]);
    assert_eq!(runner.runtime().frames, [frame]);

    let mut runner = artifact.instantiate_static(Services::default()).unwrap();
    let mut frame = Frame {
        address: address(0),
        remaining_budget: 1,
        ..Frame::default()
    };
    assert_eq!(runner.run(&mut frame).unwrap(), Exit::Budget);
    assert_eq!(
        (frame.gpr[0], frame.charged_work, frame.remaining_budget),
        (1, 3, 0)
    );
    assert_eq!(runner.runtime().calls, 0);
}

#[test]
fn native_admission_error_traps_before_effects_and_never_calls_fallback() {
    let artifact = admitted_artifact();
    let mut runner = artifact
        .instantiate_static(AdmissionServices {
            fail: true,
            ..AdmissionServices::default()
        })
        .unwrap();
    let initial = Frame {
        address: address(0),
        remaining_budget: 100,
        charged_work: 42,
        ..Frame::default()
    };
    let mut frame = initial;
    assert!(
        runner
            .run(&mut frame)
            .unwrap_err()
            .contains("test admission failure")
    );
    assert_eq!(frame, initial);
    assert_eq!(runner.runtime().admissions, [(0, 3)]);
    assert!(runner.runtime().frames.is_empty());
    assert!(runner.run(&mut frame).unwrap_err().contains("stopped"));
    assert_eq!(runner.runtime().admissions.len(), 1);
}
