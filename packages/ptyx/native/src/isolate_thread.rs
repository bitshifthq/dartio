use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

struct Invocation<F, R> {
    operation: Option<F>,
    result: Option<R>,
}

pub(crate) fn run<F, R>(operation: F) -> Option<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    extern "C" fn invoke<F, R>(context: *mut c_void) -> *mut c_void
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let invocation = unsafe { &mut *context.cast::<Invocation<F, R>>() };
        let Some(operation) = invocation.operation.take() else {
            return ptr::null_mut();
        };
        invocation.result = catch_unwind(AssertUnwindSafe(operation)).ok();
        ptr::null_mut()
    }

    let mut invocation = Box::new(Invocation {
        operation: Some(operation),
        result: None,
    });
    let mut thread = std::mem::MaybeUninit::<libc::pthread_t>::uninit();
    let created = unsafe {
        libc::pthread_create(
            thread.as_mut_ptr(),
            ptr::null(),
            invoke::<F, R>,
            (&mut *invocation as *mut Invocation<F, R>).cast(),
        )
    };
    if created != 0 {
        return None;
    }
    if unsafe { libc::pthread_join(thread.assume_init(), ptr::null_mut()) } != 0 {
        // A failed join does not prove the native thread stopped. Preserve its
        // invocation storage so the thread can never dereference freed memory.
        let _ = Box::into_raw(invocation);
        return None;
    }
    invocation.result.take()
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn returns_the_thread_result() {
        assert_eq!(run(|| 7), Some(7));
    }
}
