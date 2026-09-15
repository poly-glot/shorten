use lambda_http::{Request, run, service_fn};
use redirect::Redirect;
use shared::table::DynamoRepo;

#[tokio::main]
async fn main() -> Result<(), lambda_http::Error> {
    shared::telemetry::init_logging();
    let handler = Redirect::new(DynamoRepo::from_env().await?);
    let handler = &handler;

    run(service_fn(async move |request: Request| {
        Ok::<_, lambda_http::Error>(handler.handle(request).await)
    }))
    .await
}
