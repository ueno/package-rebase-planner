mod agent;
mod app;
mod git;
mod model;
mod progress;
mod prompts;
mod tools;

fn main() {
    if let Err(error) = app::run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
