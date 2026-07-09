#!/bin/bash -e
# pi-gen stage prerun — start from the previous stage's rootfs (standard boilerplate).
if [ ! -d "${ROOTFS_DIR}" ]; then
	copy_previous
fi
