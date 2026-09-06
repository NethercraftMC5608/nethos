# nk -- the NETHOS kernel.
#
# cargo builds the Rust; this makes the flat binary QEMU wants and, from
# Stage 2 on, drives ldk over the C shim. `scripts/run-kernel.sh` calls cargo
# directly and does not need this file -- it is here for the parts cargo
# cannot express.

TARGET  := aarch64-unknown-none-softfloat
PROFILE ?= release
OUT     := target/$(TARGET)/$(PROFILE)
ELF     := $(OUT)/nk
BIN     := $(ELF).bin

# rustup installs llvm-objcopy inside the toolchain rather than on PATH, and
# macOS's own objcopy is cctools' and cannot read ELF at all.
OBJCOPY := $(shell command -v llvm-objcopy 2>/dev/null || \
             ls $(HOME)/.rustup/toolchains/*/lib/rustlib/*/bin/llvm-objcopy 2>/dev/null | head -1)

CARGOFLAGS := $(if $(filter release,$(PROFILE)),--release,)

.PHONY: all clean run test syms fmt
all: $(BIN)

$(ELF): $(shell find core/src -type f) core/linker.ld core/Cargo.toml
	cargo build $(CARGOFLAGS)

$(BIN): $(ELF)
	@test -n "$(OBJCOPY)" || { echo "llvm-objcopy missing: rustup component add llvm-tools" >&2; exit 1; }
	$(OBJCOPY) -O binary $< $@
	@echo "  $(BIN)  $$(wc -c <$@ | tr -d ' ') bytes"

run: $(BIN)
	../scripts/run-kernel.sh --no-build

test:
	cd .. && python3 -m unittest discover -s tests -p 'test_kernel_*.py' -v

fmt:
	cargo fmt

clean:
	cargo clean
