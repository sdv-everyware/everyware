# fbgrab

Rust tool that dumps the active primary-plane DRM framebuffer of `/dev/dri/card0` (or another
card with `-d`) to a tightly packed RGBA8888 raw file, for devices without SurfaceFlinger where
`screencap` does not exist.

## Build

```bash
# RB3 (arm64)
source device/rb3/sdv_media_db845c/tools/env.sh
m fbgrab

# Cuttlefish (x86_64) and host unit tests
source device/cf-everyware/sdv_media_everyware_cf/tools/env.sh
m fbgrab fbgrab_test
cuttlefish_out/host/linux-x86/nativetest64/fbgrab_test/fbgrab_test
```

## Use on a device

Requires root (`CAP_SYS_ADMIN`) so `DRM_IOCTL_MODE_GETFB2` returns the GEM handle of the
scanout buffer.

```bash
adb push <out>/target/product/<device>/system/bin/fbgrab /data/local/tmp/
adb shell 'rm -f /data/local/tmp/fb*.raw; /data/local/tmp/fbgrab /data/local/tmp/fb'
adb pull "/data/local/tmp/$(adb shell 'cd /data/local/tmp; ls fb.*.raw' | tr -d '\r')" fb.raw
python3 -c "from PIL import Image; d=open('fb.raw','rb').read()[:1920*1080*4]; Image.frombytes('RGBA',(1920,1080),d).convert('RGB').save('screen.png')"
```

Each active primary framebuffer is written to `<prefix>.<fb_id>.raw` and logged with its
dimensions, FourCC, pitch, offset and modifier. The framebuffer must be linear (`modifier=0x0`;
on the RB3, `vendor.minigbm.debug=nocompression` ensures linear scanout buffers).

Note: on Cuttlefish with `gfxstream`, GPU rendering stays in host memory, so use
`device/cf-everyware/sdv_media_everyware_cf/tools/run.sh screenshot <file.png>` for Cuttlefish
panel screenshots; `fbgrab` reads guest-memory KMS scanout buffers (RB3, Raspberry Pi 5, or
dumb framebuffers).
