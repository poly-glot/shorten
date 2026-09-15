use chrono::Utc;
use lambda_runtime::{LambdaEvent, run, service_fn};
use rollup::athena::AthenaSource;
use rollup::{Rollup, RollupEvent, Summary, target_date};
use shared::table::DynamoRepo;

#[tokio::main]
async fn main() -> Result<(), lambda_runtime::Error> {
    shared::telemetry::init_logging();
    let handler = Rollup::new(DynamoRepo::from_env().await?, AthenaSource::from_env().await?);
    let handler = &handler;

    run(service_fn(
        async move |event: LambdaEvent<RollupEvent>| -> Result<Summary, lambda_runtime::Error> {
            handler.run(target_date(&event.payload, Utc::now())).await.map_err(lambda_runtime::Error::from)
        },
    ))
    .await
}
