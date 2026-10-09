use super::*;
use std::sync::mpsc;

#[test]
fn replacement_drains_all_inference_and_blocks_new_calls_until_cleanup_finishes() {
    let gate = InferenceGate::default();
    let first = gate.enter();
    let second = gate.enter();
    let (replaced, observe_replacement) = mpsc::channel();
    let (finish, finish_cleanup) = mpsc::channel();
    let (entered, observe_entry) = mpsc::channel();
    std::thread::scope(|scope| {
        let replacement_gate = &gate;
        scope.spawn(move || {
            let _replacement = replacement_gate.replace();
            replaced.send(()).expect("replacement starts");
            finish_cleanup.recv().expect("cleanup completes");
        });
        let mut state = gate.state.lock().expect("gate state");
        while !state.replacing {
            state = gate.changed.wait(state).expect("pending replacement");
        }
        assert_eq!(state.active, 2);
        drop(state);
        scope.spawn(|| {
            let _inference = gate.enter();
            entered.send(()).expect("new inference starts");
        });
        drop(first);
        assert_eq!(gate.state.lock().expect("gate state").active, 1);
        assert!(matches!(
            observe_replacement.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(second);
        observe_replacement.recv().expect("all GPU calls drained");
        assert_eq!(gate.state.lock().expect("gate state").active, 0);
        assert!(matches!(
            observe_entry.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        finish.send(()).expect("release cleanup");
        observe_entry
            .recv()
            .expect("replacement permits new inference");
    });
    assert_eq!(gate.state.lock().expect("gate state").active, 0);
    let failed_replacement: Result<(), &str> = {
        let _replacement = gate.replace();
        Err("replacement failure")
    };
    assert!(failed_replacement.is_err());
    let _next = gate.enter();
    assert_eq!(gate.state.lock().expect("recovered gate").active, 1);
}
