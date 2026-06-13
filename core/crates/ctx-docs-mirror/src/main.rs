#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ctx_docs_mirror::run().await
}
