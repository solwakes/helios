KERNEL := target/riscv64gc-unknown-none-elf/release/helios

.PHONY: build run run-gui clean

build:
	cargo build --release

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
		-device virtio-gpu-device \
		-kernel $(KERNEL)

clean:
	cargo clean
