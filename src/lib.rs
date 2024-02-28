//! Improved tonic interceptor
#![warn(missing_docs)]
#![cfg_attr(feature = "cargo-clippy", allow(clippy::style))]

use core::task;
use core::pin::Pin;
use core::future::Future;

///Tonic interceptor
pub trait Interceptor {
    ///Callback on incoming request, allowing you to modify headers or extensions
    ///
    ///Note that under the hood tonic types are the same as `http` types so even though it is `http::Extensions`, it is in fact the same shit
    ///
    ///Returning status will preempt request handling and immediately returns status
    fn on_request(&self, headers: &mut tonic::metadata::MetadataMap, extensions: &mut http::Extensions) -> Option<tonic::Status>;

    #[inline(always)]
    ///Callback when response is being returned
    ///
    ///By default does nothing
    fn on_response(&self, _headers: &mut tonic::metadata::MetadataMap, _extensions: &http::Extensions) {
    }
}

impl<I: Interceptor> Interceptor for std::sync::Arc<I> {
    #[inline(always)]
    fn on_request(&self, headers: &mut tonic::metadata::MetadataMap, extensions: &mut http::Extensions) -> Option<tonic::Status> {
        Interceptor::on_request(self.as_ref(), headers, extensions)
    }

    #[inline(always)]
    fn on_response(&self, headers: &mut tonic::metadata::MetadataMap, extensions: &http::Extensions) {
        Interceptor::on_response(self.as_ref(), headers, extensions)
    }
}

///Layer
#[derive(Clone)]
#[repr(transparent)]
pub struct InterceptorLayer<I>(I);

impl<S, I: Interceptor + Clone> tower_layer::Layer<S> for InterceptorLayer<I> {
    type Service = InterceptorService<I, S>;

    #[inline(always)]
    fn layer(&self, inner: S) -> Self::Service {
        InterceptorService::new(self.0.clone(), inner)
    }
}

///Service
pub struct InterceptorService<I, S> {
    interceptor: I,
    inner: S
}

impl<I, S> InterceptorService<I, S> {
    #[inline(always)]
    ///Creates new instance
    pub fn new(interceptor: I, inner: S) -> Self {
        Self {
            interceptor,
            inner
        }
    }
}

impl<ReqBody, ResBody: Default, S: tower_service::Service<http::Request<ReqBody>, Response = http::Response<ResBody>>, I: Interceptor + Clone> tower_service::Service<http::Request<ReqBody>> for InterceptorService<I, S> {
    type Response = S::Response;
    type Error = S::Error;
    type Future = InterceptorFut<I, S::Future>;

    #[inline(always)]
    fn poll_ready(&mut self, cx: &mut task::Context<'_>) -> task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    #[inline(always)]
    fn call(&mut self, mut req: http::Request<ReqBody>) -> Self::Future {
        let (mut parts, body) = req.into_parts();

        let mut headers = tonic::metadata::MetadataMap::from_headers(parts.headers);
        match self.interceptor.on_request(&mut headers, &mut parts.extensions) {
            None => {
                parts.headers = headers.into_headers();
                req = http::Request::from_parts(parts, body);
                InterceptorFut::fut(self.interceptor.clone(), self.inner.call(req))
            }
            Some(status) => InterceptorFut::status(self.interceptor.clone(), status),
        }
    }
}

///Interception service future
pub struct InterceptorFut<I, F> {
    interceptor: I,
    inner: Result<F, tonic::Status>,
}

impl<I, F> InterceptorFut<I, F> {
    #[inline(always)]
    fn status(interceptor: I, status: tonic::Status) -> Self {
        Self {
            interceptor,
            inner: Err(status),
        }
    }

    #[inline(always)]
    fn fut(interceptor: I, fut: F) -> Self {
        Self {
            interceptor,
            inner: Ok(fut),
        }
    }
}


impl<ResBody: Default, E, I: Interceptor, F: Future<Output = Result<http::Response<ResBody>, E>>> Future for InterceptorFut<I, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, ctx: &mut task::Context<'_>) -> task::Poll<Self::Output> {
        let (intercepter, fut) = unsafe {
            let this = self.get_unchecked_mut();
            let fut = match this.inner.as_mut() {
                Ok(fut) => Pin::new_unchecked(fut),
                Err(status) => {
                    let mut resp = http::Response::new(Default::default());
                    resp.headers_mut().insert(http::header::CONTENT_TYPE, http::header::HeaderValue::from_static("application/grpc"));
                    let _ = status.add_header(resp.headers_mut());
                    return task::Poll::Ready(Ok(resp));
                }
            };
            (&this.interceptor, fut)
        };
        match Future::poll(fut, ctx) {
            task::Poll::Ready(Result::Ok(resp)) => {
                let (mut parts, body) = resp.into_parts();
                let mut headers = tonic::metadata::MetadataMap::from_headers(parts.headers);
                intercepter.on_response(&mut headers, &parts.extensions);
                parts.headers = headers.into_headers();
                task::Poll::Ready(Ok(http::Response::from_parts(parts, body)))
            },
            task::Poll::Ready(Result::Err(error)) => task::Poll::Ready(Err(error)),
            task::Poll::Pending => task::Poll::Pending,
        }
    }
}

#[derive(Clone)]
///Interceptor for on request only
pub struct OnRequest<F>(pub F);

impl<F: Fn(&mut tonic::metadata::MetadataMap, &mut http::Extensions) -> Option<tonic::Status>> Interceptor for OnRequest<F> {
    #[inline(always)]
    fn on_request(&self, headers: &mut tonic::metadata::MetadataMap, extensions: &mut http::Extensions) -> Option<tonic::Status> {
        (self.0)(headers, extensions)
    }
}

#[derive(Clone)]
///Utility to define interceptor using function pointers
pub struct InterceptorFn<OnReq, OnResp> {
    ///Callback to be called on incoming request
    pub on_request: OnReq,
    ///Callback to be called on response ready
    pub on_response: OnResp,
}

impl<OnReq: Fn(&mut tonic::metadata::MetadataMap, &mut http::Extensions) -> Option<tonic::Status>, OnResp: Fn(&mut tonic::metadata::MetadataMap, &http::Extensions)> Interceptor for InterceptorFn<OnReq, OnResp> {

    #[inline(always)]
    fn on_request(&self, headers: &mut tonic::metadata::MetadataMap, extensions: &mut http::Extensions) -> Option<tonic::Status> {
        (self.on_request)(headers, extensions)
    }

    #[inline(always)]
    fn on_response(&self, headers: &mut tonic::metadata::MetadataMap, extensions: &http::Extensions) {
        (self.on_response)(headers, extensions)
    }
}

#[inline(always)]
///Creates interceptor layer
pub fn interceptor<I: Interceptor>(interceptor: I) -> InterceptorLayer<I> {
    InterceptorLayer(interceptor)
}
