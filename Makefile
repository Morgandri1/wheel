# QA owns `check`; teams add their own targets under their area.
.PHONY: check
check:
	@echo "make check: QA has not landed this yet — run your crate/package tests directly." && exit 1
