use tonic_interceptor::{OnRequest, InterceptorService};

use tonic::Status;
use tonic::metadata::{MetadataValue, MetadataMap};
use tower_service::Service;

use core::task;
use core::pin::pin;
use core::future::{self, Future};

mod noop {
    use core::{ptr, task};

    const VTABLE: task::RawWakerVTable = task::RawWakerVTable::new(clone, action, action, action);
    const WAKER: task::RawWaker = task::RawWaker::new(ptr::null(), &VTABLE);

    fn clone(_: *const()) -> task::RawWaker {
        WAKER
    }

    fn action(_: *const ()) {
    }

    #[inline(always)]
    pub fn waker() -> task::Waker {
        unsafe {
            task::Waker::from_raw(WAKER)
        }
    }
}

#[derive(Copy, Clone)]
pub struct ServiceFn<T>(T);

impl<Request, R, E, T: FnMut(Request) -> Result<R, E>> Service<Request> for ServiceFn<T> {
    type Response = R;
    type Error = E;
    type Future = future::Ready<Result<R, E>>;

    fn poll_ready(&mut self, _: &mut task::Context<'_>) -> task::Poll<Result<(), E>> {
        Ok(()).into()
    }

    fn call(&mut self, req: Request) -> Self::Future {
        future::ready((self.0)(req))
    }
}

#[test]
fn should_propagate_status_on_request() {
    const MSG: &str = "BAD";
    let expected = Status::permission_denied(MSG).to_http();

    let svc = ServiceFn(|_: http::Request<()>| {
        Ok::<_, Status>(http::Response::new(()))
    });

    let interceptor = OnRequest(|_: &mut tonic::metadata::MetadataMap, _: &mut http::Extensions| {
        Some(Status::permission_denied(MSG))
    });

    let mut service = InterceptorService::new(interceptor, svc);
    let request = http::Request::builder().body(()).unwrap();
    let res = pin!(service.call(request));

    let waker = noop::waker();
    let mut ctx = task::Context::from_waker(&waker);

    let response = match Future::poll(res, &mut ctx) {
        task::Poll::Ready(result) => result.expect("Response"),
        task::Poll::Pending => unreachable!(),
    };

    assert_eq!(expected.status(), response.status());
    assert_eq!(expected.version(), response.version());
    assert_eq!(expected.headers(), response.headers());
}

#[test]
fn should_modify_request_parts() {
    struct Dummy(&'static str);

    const MSG: &str = "BAD";
    const BIN: &str = "BIN";
    const EXT: &str = "EXT";
    let expected = http::Response::new(());

    let svc = ServiceFn(|req: http::Request<()>| {
        assert_eq!(req.extensions().len(), 1);
        let dummy = req.extensions().get::<Dummy>().expect("To have Dummy extensions");
        assert_eq!(dummy.0, EXT);

        let (parts, _) = req.into_parts();
        let headers = MetadataMap::from_headers(parts.headers);

        let bin = headers.get_bin("x-msg-bin").expect("to have x-msg-bin").to_bytes().expect("to convert bin");
        assert_eq!(BIN.as_bytes(), bin);

        let msg = headers.get("x-msg").expect("to have x-msg");
        assert_eq!(msg.as_bytes(), MSG.as_bytes());

        Ok::<_, Status>(http::Response::new(()))
    });

    let interceptor = OnRequest(|headers: &mut MetadataMap, extensions: &mut http::Extensions| {
        headers.insert_bin("x-msg-bin", MetadataValue::from_bytes(BIN.as_bytes()));
        headers.insert("x-msg", MSG.parse().unwrap());
        extensions.insert(Dummy(EXT));
        None
    });

    let mut service = InterceptorService::new(interceptor, svc);
    let request = http::Request::builder().body(()).unwrap();
    let res = pin!(service.call(request));

    let waker = noop::waker();
    let mut ctx = task::Context::from_waker(&waker);

    let response = match Future::poll(res, &mut ctx) {
        task::Poll::Ready(result) => result.expect("Response"),
        task::Poll::Pending => unreachable!(),
    };

    assert_eq!(expected.status(), response.status());
    assert_eq!(expected.version(), response.version());
    assert_eq!(expected.headers(), response.headers());
}
