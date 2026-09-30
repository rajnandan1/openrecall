# OpenRecall

- The binary is Rust. `eval/` is the one exception: Python 3, standard library only.
- Nothing private is ever committed: no ticket IDs, customer names, memory contents, real prompts or transcript excerpts. Eval data lives only in `~/.openrecall/eval/`, and tests use made-up transcripts. Jev is called only through `judge.py` from the harness scripts, never from the binary.
