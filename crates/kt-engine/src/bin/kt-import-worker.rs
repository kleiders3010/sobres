//! Disposable import worker (D3). Started by the host with `--kt-import-worker`.
fn main() {
    kt_engine::sandbox::worker_main();
}
