CARGO := $(HOME)/.cargo/bin/cargo
KERNEL := target/riscv64gc-unknown-none-elf/release/helios
DISK := helios.img

.PHONY: build run run-gui clean

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
		-global virtio-mmio.force-legacy=false \
		-kernel $(KERNEL)

run-gui: build $(DISK)
	qemu-system-riscv64 \
		-machine virt \
		-bios default \
		-serial stdio \
		-device ramfb \
		-device virtio-keyboard-device \
		-drive file=$(DISK),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-kernel $(KERNEL)

clean:
	$(CARGO) clean
