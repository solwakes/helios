CARGO := $(HOME)/.cargo/bin/cargo
KERNEL := target/riscv64gc-unknown-none-elf/release/helios
DISK := helios.img

.PHONY: build run run-gui clean test-host

build:
	$(CARGO) build --release

$(DISK):
	qemu-img create -f raw $(DISK) 16M

run: build $(DISK)
	qemu-system-riscv64 \
		-machine virt \
		-nographic \
		-bios default \
		-serial mon:stdio \
		-drive file=$(DISK),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0,hostfwd=tcp::5555-:80 \
		-device virtio-net-device,netdev=net0 \
		-global virtio-mmio.force-legacy=false \
		-kernel $(KERNEL)

run-gui: build $(DISK)
	qemu-system-riscv64 \
		-machine virt \
		-bios default \
		-serial stdio \
		-device ramfb \
		-device virtio-keyboard-device \
		-device virtio-tablet-device \
		-drive file=$(DISK),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0,hostfwd=tcp::5555-:80 \
		-device virtio-net-device,netdev=net0 \
		-global virtio-mmio.force-legacy=false \
		-kernel $(KERNEL)

clean:
	$(CARGO) clean

# Run host-side unit tests for helios-std (Label/Errno encoding,
# NodeId display, edge wire-format decode, syscall-number constants).
# Tests compile against the host triple — see scripts/test-host.sh.
test-host:
	./scripts/test-host.sh
