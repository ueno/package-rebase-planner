//! System prompts and opening messages.
//!
//! Kept apart from the loop because these are the part that actually determines
//! quality, and they are edited far more often than the machinery around them.

use crate::tools::Assessment;

/// Instructions for assessing risk of a given commit
pub const ASSESS: &str = r#"You analyze a given Git commit, by classifying its type and grading the impact of it.

To classify the type, follow the Conventional Commits guildeline:

- `feat` if the commit adds a new, user visible feature. NOTE: additions or changes to the internal interface, such as internal library API or header files, is NOT CONSIDERED a `feat` but a `refactor` or a `fix`
- `fix` if the commit represents a bug fix
- `refactor` if the commit refactors the exisiting code including tests
- `test` if the commit adds tests or extends the existing tests. NOTE: refactoring of tests should be marked as `refactor` instead of `test`
- `docs` if the commit only adds documentation for the library or application code
- `chore` if the commit only touches files for maintenance, such GitHub actions (.github/) or GitLab CI (.gitlab-ci.yml), release steps
- `style` if the commit only changes the coding style
- `perf` if the commit is mainly for performance improvements

To grade the impact, use an integer between 1 and 5, where 1 is the safest:

- 1 if the commit is safe to pick without any further adjustment or testing
- 2 if the commit is generally safe: needs additional testing
- 3 if the commit is neutrally safe: needs adjustment in code and additional testing
- 4 if the commit is risky: no backward compatibility concerns, but adds new functions or changes library behavior, or update of configuration files is needed
- 5 if the commit is a breaking change: this commit needs to be reverted to preserve compatibility

YOU MUST COMPLETE BOTH CLASSIFYING AND GRADING.

HOW TO WORK:
1. Read the content of the given commit, using `read_commit` tool. IF AND ONLY IF you can't assess the commit, you can use `read_file` to read a file content. Since this is an expensive operation, you should generally avoid that.
2. Once you've done with your analysis, call `write_assessment` tool to record it.
3. Summarize what you did and why in plain prose, briefly.

If the given commit fixes a bug or adds a feature, but no test case is
included in the commit, check the adjacent commits using
`list_commits` tool, followed by `read_commit`.

After calling `list_commits`, if you notice there are similar commits,
you can call `read_assessment` to check any existing assessment.

IMPORTANT:
NEVER CALL THE SAME TOOL MORE THAN 3 TIMES ON THE SAME COMMIT. If you
haven't read the entire content, it's OK. Make your assessment based
on what you have already read, even if it is partial.

When assessing backward compatibility, ONLY CONSIDER THE GIVEN BUILD
CONFIGURATION. Read `configure.ac` or `m4/*` with `read_file` to check
if the commit actually affects the given build configuration.

Call `finish` when you've done with it."#;

/// The opening message for assessment.
pub fn assess_opening(commit: &str, hints: Option<&str>) -> String {
    let mut text = format!(
        "Commit: {commit}\n\nRead the commit's content, classify the type and grade the impact."
    );
    if let Some(hints) = hints {
        text.push_str(&format!("\n\nHints:\n{hints}"));
    }
    text
}

/// Instructions for planning rebase based on the given assessment
pub const PLAN: &str = r#"You propose a concrete remediation plan, based on the given assessment for commits with higher impact.

For each commit you propose either:

- Creating additional tests to ensure the correctness of the commit,
- Extending the documentation (release notes) to notify the user,
- Disabing the feature with a compile-time flag,
- Completely reverting the commit if there is no way to disable it easily

HOW TO WORK:
1. Read the content of the given commit, using `read_commit` tool.
2. If you need additional context, call `list_commits` to list adjacent commits, followed by either `read_commit`, `read_assessment`, or `read_recommendation`. NEVER CALL THE SAME TOOL MORE THAN 3 TIMES ON THE SAME COMMIT.
3. Write your recommended actions, using `write_recommendations` tool.
4. Summarize what you did and why in plain prose, briefly.

IMPORTANT:
- Briefness is the key. Explain summary in one sentence without headings.
- Output should be in markdown format.

Call `finish` when you've done with it."#;

/// The opening message for planning.
pub fn plan_opening(commit: &str, message: &str, assessment: &Assessment, hints: Option<&str>) -> String {
    let mut text = format!(
        "Propose a remediation plan for the given risky commit:\n\n- {}: {}\n  type: {:?}\n  impact: {}\n  rationale: {}\n",
        commit,
        message,
        assessment.ty,
        assessment.impact,
        assessment.rationale.as_ref().map_or("", |v| v),
    );
    if let Some(hints) = hints {
        text.push_str(&format!("\n\nHints:\n{hints}"));
    }
    text
}
