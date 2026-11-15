#[cfg(feature = "wasm-adapter")]
use crate::runtime::Status;
use crate::runtime::{EngineError, Session};
// Private ABI v1: session is borrowed exclusively for each callback. Scalars use
// i64 bits (binary64 for REAL). Helpers never retain pointers or unwind into JIT.
#[cfg(feature = "wasm-adapter")]
pub(super) unsafe extern "C" fn state(session: *mut Session) -> i64 {
    // SAFETY: only the artifact entry calls helpers with its live session pointer.
    let s = unsafe { &mut *session };
    if let Some(report) = s.execution_report_mut() {
        report.boundaries.state += 1;
    }
    if matches!(
        s.status(),
        Status::Finished | Status::Failed(_) | Status::Waiting
    ) || (*s.status() == Status::WaitingForInput && s.context.input.is_empty())
    {
        -1
    } else {
        s.state_key()
    }
}
pub(super) unsafe extern "C" fn step(session: *mut Session) {
    let s = unsafe { &mut *session };
    if let Some(report) = s.execution_report_mut() {
        report.boundaries.step += 1;
    }
    let span = s.current_span().unwrap_or_default();
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.step())).is_err() {
        s.fail(EngineError::Panic, span);
    }
}
#[cfg(feature = "wasm-adapter")]
pub(super) unsafe extern "C" fn get(session: *mut Session, r: i64) -> i64 {
    let s = unsafe { &mut *session };
    if let Some(report) = s.execution_report_mut() {
        report.boundaries.get += 1;
    }
    s.scalar_bits(r as usize)
}
#[cfg(feature = "wasm-adapter")]
pub(super) unsafe extern "C" fn put(session: *mut Session, r: i64, bits: i64, overflow: i64) {
    let s = unsafe { &mut *session };
    if let Some(report) = s.execution_report_mut() {
        report.boundaries.put += 1;
    }
    unsafe {
        (&mut *session).native_result(r as usize, bits, overflow != 0);
    }
}

// Native safepoints materialize scalars before Rust can inspect or change frames.
// No pointer loaded from NativeView may be carried across either callback.
pub(super) unsafe extern "C" fn native_step(session: *mut Session) {
    let s = unsafe { &mut *session };
    s.finish_native();
    unsafe { step(session) };
    unsafe { &mut *session }.prepare_native();
}

pub(super) unsafe extern "C" fn native_flush(session: *mut Session) {
    let s = unsafe { &mut *session };
    s.synchronize_native_return();
}
pub(super) unsafe extern "C" fn native_enter(session: *mut Session) {
    let s = unsafe { &mut *session };
    s.enter_native_reporting();
}
pub(super) unsafe extern "C" fn native_property_write(session: *mut Session) {
    let s = unsafe { &mut *session };
    s.finish_native();
    if !matches!(s.status(), crate::runtime::Status::Paused) {
        s.prepare_native();
        return;
    }
    let span = s.current_span().unwrap_or_default();
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.native_property_step())).is_err()
    {
        s.fail(EngineError::Panic, span);
    }
    s.prepare_native();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_materialization_cannot_commit_a_later_property_write() {
        let mut session = Session::new(std::sync::Arc::new(crate::compile("").unwrap())).unwrap();
        let view = session.prepare_native();
        // SAFETY: establish the same validated engine tag as a native entry.
        unsafe {
            (*view).engine = crate::metrics::ExecutionEngine::Winch as u64;
        }
        session.fail(
            crate::runtime::ArithmeticError::IntegerOverflow,
            crate::Span::default(),
        );
        let failed = session.status().clone();
        // SAFETY: the callback borrows this live Session exclusively. A failed
        // safepoint must exit before reading any property-handler operands.
        unsafe { native_property_write(&mut session) };
        assert_eq!(session.status(), &failed);
        assert_eq!(session.instruction_count(), 0);
    }
}
