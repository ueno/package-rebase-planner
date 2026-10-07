use crate::tools::{Assessment, CommitType, Recommendation};
use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{env, fs, io::Write, path::Path};

#[derive(Parser)]
#[command(
    name = "prp",
    about = "A helper program for planning a package rebase",
    after_help = "Run `prp <command> --help` for details on any command.",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: CommandName,

    /// Print verbose output.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Subcommand)]
enum CommandName {
    /// Create a new prp configuration in a repository.
    ///
    /// Creates configuration files under `~/.config/prp/`. Safe to re-run:
    /// existing files are left alone.
    #[command(visible_alias = "i")]
    Init,

    /// Assess the impact of rebase.
    ///
    /// Examines the git commits in a given commit range, grades each
    /// commit based on multiple perspectives, and summarize the final
    /// risk of the rebase.
    ///
    ///   prp assess 3.8.11..3.8.13
    #[command(visible_alias = "a")]
    Assess {
        /// Commit range to assess, in gitrevisions(7) format
        range: String,
        /// Build configuration, e.g., config.log
        #[arg(short = 'c')]
        build_config: Option<PathBuf>,
        /// Output file name
        #[arg(short = 'o', default_value = "report.csv")]
        output: PathBuf,
    },

    /// Create a rebase plan.
    ///
    /// From the risk assessment in a CSV file, create a concrete plan
    /// of the rebase.
    ///
    ///   prp plan report.csv
    #[command(visible_alias = "p")]
    Plan {
        /// Input file name
        #[arg(default_value = "report.csv")]
        input: PathBuf,
        /// Output file name
        #[arg(short = 'o', default_value = "report.md")]
        output: PathBuf,
        /// Commit type
        #[arg(short = 'c', value_enum)]
        commit_type: Vec<CommitType>,
        /// Impact threashold
        #[arg(short = 't', default_value_t = 3)]
        threshold: usize,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let root = env::current_dir()?;
    let home = env::home_dir()
        .ok_or_else(|| anyhow!("unable to determine home directory"))
        .map(|home| home.join(".config").join("prp"))?;
    match cli.command {
        CommandName::Init => init(&home),
        CommandName::Assess {
            range,
            build_config,
            output,
        } => assess(&home, &root, cli.verbose, &range, build_config, &output),
        CommandName::Plan {
            input,
            output,
            commit_type,
            threshold,
        } => plan(
            &home,
            &root,
            cli.verbose,
            &input,
            &output,
            commit_type.as_slice(),
            threshold,
        ),
    }
}

fn init(home: &Path) -> Result<()> {
    fs::create_dir_all(home.join("providers"))?;
    if !home.join("config.toml").exists() {
        fs::write(
            home.join("config.toml"),
            concat!(
                "# prp configuration\n",
                "#\n",
                "# Each task has its own model. Assessment is short and\n",
                "# read-only, so it is the place to spend less, or to keep local.\n",
                "#\n",
                "# provider: provider name | none\n",
                "# name: model name; omit for the provider's own default.\n",
                "\n",
                "[assess]\n",
                "provider = \"anthropic\"\n",
                "name = \"claude-sonnet-4-5\"\n",
                "\n",
                "[plan]\n",
                "provider = \"anthropic\"\n",
                "name = \"claude-sonnet-4-5\"\n",
            ),
        )?;
    }
    println!("Initialized prp configuration at {}", home.display());
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct Row<'a> {
    commit: &'a str,
    message: &'a str,
    #[serde(rename = "type")]
    ty: CommitType,
    impact: usize,
    rationale: &'a str,
}

#[derive(Deserialize, Serialize)]
struct RowOwned {
    commit: String,
    message: String,
    #[serde(rename = "type")]
    ty: CommitType,
    impact: usize,
    rationale: String,
}

fn assess(
    home: &Path,
    root: &Path,
    verbose: bool,
    range: &str,
    build_config: Option<PathBuf>,
    output: &Path,
) -> Result<()> {
    let settings = crate::model::ModelSettings::load(home, crate::model::Task::Assess)?;
    let backend = crate::model::build(&settings, home)?;

    let commits = crate::git::list_commits(root, range)?;
    let workspace = crate::tools::Workspace::new(root)?;
    let workspace = Arc::new(Mutex::new(workspace));

    let build_config = if let Some(build_config) = build_config {
        Some(fs::read_to_string(&build_config)?)
    } else {
        None
    };

    let mut agent = crate::agent::Agent::new(backend, crate::agent::ASSESS);
    agent.reporter = std::sync::Arc::new(crate::progress::Reporter::new());

    let default_assessment = Assessment {
        ty: CommitType::Chore,
        impact: 0,
        rationale: None,
    };
    let mut writer = csv::WriterBuilder::new().from_path(output)?;

    for (count, (commit, message)) in commits.iter().rev().enumerate() {
        agent
            .reporter
            .started("analyzing", count + 1, commits.len(), commit, message);
        let opening = crate::prompts::assess_opening(commit, build_config.as_deref());
        let outcome = match agent.run(
            &format!("prp-session-{commit}"),
            workspace.clone(),
            &opening,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                agent.reporter.warn(&format!("error: {error:#}"));
                continue;
            }
        };

        let mut workspace = workspace.lock().unwrap();

        let assessment = workspace
            .assessments
            .entry(commit.to_string())
            .and_modify(|assessment| {
                assessment
                    .rationale
                    .replace(outcome.result.trim().to_string());
            })
            .or_insert(default_assessment.clone());

        writer.serialize(Row {
            commit,
            message,
            ty: assessment.ty,
            impact: assessment.impact,
            rationale: assessment.rationale.as_deref().unwrap_or(""),
        })?;
        writer.flush()?;

        if verbose {
            eprintln!("{}", outcome.result.trim());

            eprintln!(
                "{} turn{}, {} in / {} out tokens",
                outcome.turns,
                if outcome.turns == 1 { "" } else { "s" },
                outcome.usage.input_tokens.unwrap_or(0),
                outcome.usage.output_tokens.unwrap_or(0)
            );
        }
    }

    Ok(())
}

fn plan(
    home: &Path,
    root: &Path,
    verbose: bool,
    input: &Path,
    output: &Path,
    commit_type: &[CommitType],
    threshold: usize,
) -> Result<()> {
    let settings = crate::model::ModelSettings::load(home, crate::model::Task::Plan)?;
    let backend = crate::model::build(&settings, home)?;

    let mut workspace = crate::tools::Workspace::new(root)?;

    let mut agent = crate::agent::Agent::new(backend, crate::agent::PLAN);
    agent.reporter = std::sync::Arc::new(crate::progress::Reporter::new());

    let mut output = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(output)?;

    let mut assessments = vec![];

    let mut reader = csv::ReaderBuilder::new().from_path(input)?;
    for result in reader.deserialize() {
        let record: RowOwned = result?;
        let assessment = Assessment {
            ty: record.ty,
            impact: record.impact,
            rationale: Some(record.rationale.to_string()),
        };

        if (commit_type.is_empty() || commit_type.contains(&assessment.ty))
            && assessment.impact >= threshold
        {
            assessments.push((
                record.commit.clone(),
                record.message.clone(),
                assessment.clone(),
            ));
        }

        workspace.assessments.insert(record.commit, assessment);
    }

    assessments.sort_by_key(|(_, _, assessment)| assessment.impact);
    let workspace = Arc::new(Mutex::new(workspace));

    for (commit, message, assessment) in assessments.iter().rev() {
        let opening = crate::prompts::plan_opening(&commit, &message, &assessment);
        let outcome = match agent.run(
            &format!("prp-plan-{}", &commit),
            workspace.clone(),
            &opening,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                agent.reporter.warn(&format!("error: {error:#}"));
                continue;
            }
        };

        writeln!(
            &mut output,
            "## Commit {}: {}\n",
            commit.chars().take(9).collect::<String>(),
            &message
        )?;
        writeln!(&mut output, "{}\n", outcome.result.trim())?;
        writeln!(&mut output, "### Recommended actions:\n")?;
        let workspace = workspace.lock().unwrap();
        if let Some(recommendations) = workspace.recommendations.get(commit) {
            for recommendation in recommendations {
                match recommendation {
                    Recommendation::Test { criteria } => {
                        writeln!(&mut output, "- **Test**: {criteria}\n")?
                    }
                    Recommendation::Document { text } => {
                        writeln!(&mut output, "- **Document**: {text}\n")?
                    }
                    Recommendation::Revert { reason } => {
                        writeln!(&mut output, "- **Revert**: {reason}\n")?
                    }
                }
            }
        }

        if verbose {
            eprintln!("{}", outcome.result.trim());

            eprintln!(
                "{} turn{}, {} in / {} out tokens",
                outcome.turns,
                if outcome.turns == 1 { "" } else { "s" },
                outcome.usage.input_tokens.unwrap_or(0),
                outcome.usage.output_tokens.unwrap_or(0)
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Help text is documentation, and documentation that only exists in one
    /// place goes stale silently. This at least enforces that it exists.
    #[test]
    fn every_command_and_argument_is_documented() {
        let command = Cli::command();
        for subcommand in command.get_subcommands() {
            let name = subcommand.get_name();
            if name == "help" {
                continue;
            }
            assert!(
                subcommand.get_about().is_some(),
                "`prp {name}` has no help text"
            );
            for argument in subcommand.get_arguments() {
                let id = argument.get_id();
                if id == "help" {
                    continue;
                }
                assert!(
                    argument.get_help().is_some(),
                    "`prp {name} --{id}` has no help text"
                );
            }
        }
    }
}
