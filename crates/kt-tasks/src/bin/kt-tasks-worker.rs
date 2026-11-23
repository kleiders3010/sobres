//! Import worker for the tasks app (D3). Started by the host with `--kt-import-worker`.
fn main() {
    // the app's own readers run on the merged document too (T2-3)
    kt_engine::sandbox::worker_main_with(|d| {
        let _ = kt_tasks::tasks(d);
        let _ = kt_tasks::normalized(d);
        // and the budget's (second-pass Sobres audit S3-9 b)
        let _ = kt_tasks::budget::transactions(d);
        let _ = kt_tasks::budget::budget(d, "2026-01-01");
        let _ = kt_tasks::budget::repeats(d);
    });
}
