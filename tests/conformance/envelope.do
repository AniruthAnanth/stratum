* tests/conformance/envelope.do
*
* THIS FILE DELIBERATELY EXECUTES NOTHING, and that is what it is for.
*
* ARCHITECTURE §8.9 compares `stratum run <case> --json --deterministic` across
* macOS, Windows and Linux and requires the bytes to be identical. Everything
* that can differ between those three machines is in the run ENVELOPE rather
* than in any command's output: the absolute path of the entry file, the path
* separator, the working directory, the wall clock, the version string and the
* order in which ids are allocated. A do-file with no executable region puts
* every one of those on the wire and adds nothing else, so a diff here names the
* platform difference directly instead of burying it under a regression table.
*
* It is also the one shape whose expected stream does not change on the day the
* engine is linked: `RunStarted`, `RunFinished`, rc 0, no blocks, because there
* was nothing for an engine to do.
