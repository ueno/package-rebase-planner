# package-rebase-planner

package-rebase-planner is an LLM-backed tool to help planning a
downstream package rebase. It consists of two subcommands:

- `assess`: assesses risk of the rebase. This examines each commit in
  a given commit range, classifies them according to the [Conventional
  Commits guideline][conventional-commits], and grades the impact in a
  1-to-5 scale. Final report will be written to a CSV file with
  rational for each assessment.

- `plan`: creates action plans for the rebase according to the
  assessment. This may suggest either adding tests or expanding the
  documentation.

## Demo

[![asciicast](https://asciinema.org/a/1267552.svg)](https://asciinema.org/a/1267552)

## Usage

### Initialization

```console
$ prp init
$ edit ~/.config/prp/config.toml
```

### Assessing

```console
$ cd some-git-repository
$ prp assess v1.0..v1.1 -o report-v1.0-v1.1.csv
```

### Planning

```console
$ prp plan report-v1.0-v1.1.csv -o plan-v1.0-v1.1.md
```

## How it works

package-rebase-planner is built using the [Goose Development Kit
(GDK)][gdk], which allows to develop an application based on the agent
loop architecture.  The agents are provided with internally defined
tools as described below:

### General tools

- `read_file`: read the content of a file at a given commit (calls git)
- `read_commit`: read the content of a given commit (calls git)
- `list_commits`: list adjacent commits around a given commit (calls git)
- `finish`: finish the assessment, report the summary in a prose

### For assessment

- `write_assessment`: write assessment of a given commit
- `read_assessment`: read assessment of a given commit, if any

### For planning

- `write_recommendations`: write recommended actions for a given commit
- `read_recommendations`: read recommended actions for a given commit

## Credits

This project is heavily inspired by [lifewiki][lifewiki].

## License

MIT

[conventional-commits]: https://www.conventionalcommits.org
[gdk]: https://goose-docs.ai/docs/gdk/
[lifewiki]: https://github.com/jamadeo/lifewiki

