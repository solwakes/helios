CARGO := $(HOME)/.cargo/bin/cargo
KERNEL := target/riscv64gc-unknown-none-elf/release/helios

.PHONY: build run run-gui clean

build:
	$(CARGO) build --release

run: build
	qemu-system-riscv64 \
		-machine virt \
		-nographic \
		-bios default \
		-serial mon:stdio \
		-kernel $(KERNEL)

run-gui: build
	qemu-system-riscv64 \
		-machine virt \
		-bios default \
		-serial stdio \
		-device ramfb \
		-kernel $(KERNEL)

clean:
	$(CARGO) clean
