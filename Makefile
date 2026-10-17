# nvtop-rs — no crates, no build system beyond cargo; rustc works too.
BIN  ?= nvtop-rs
PREFIX ?= /usr/local

.PHONY: build release rustc check screenshot run clean install uninstall

release: ## cargo release build
	@cargo build --release
	@printf '\n==> target/release/%s\n' $(BIN)

build: ## debug build
	@cargo build

rustc: ## dependency-free build, one command, no cargo
	@mkdir -p build
	@rustc -O --edition 2021 -C panic=abort src/main.rs -o build/$(BIN)
	@ldd build/$(BIN)
	@printf '\n==> build/%s\n' $(BIN)

run: release ## start the monitor
	@./target/release/$(BIN)

once: release ## one text snapshot
	@./target/release/$(BIN) --once --width 132

json: release ## one JSON snapshot
	@./target/release/$(BIN) --json | python3 -m json.tool

check: release ## cross-validate the live fields against nvidia-smi, if installed
	@sh tests/compare.sh

screenshot: release ## render docs/screenshot.png (+ gif) from a real pty session
	@python3 tools/capture.py --out docs/screenshot.png --seconds 12 \
	  --cols 132 --rows 42 --title "gpucalc-01 — nvtop-rs" -- --interval 0.25 --mask

clean:
	@cargo clean
	@rm -rf build

install: release
	install -Dm755 target/release/$(BIN) $(DESTDIR)$(PREFIX)/bin/$(BIN)

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/$(BIN)
