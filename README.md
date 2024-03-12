# smoljpg
A multi-threaded image compression tool, powered by [turbojpeg](https://github.com/honzasp/rust-turbojpeg).
# Why
DSLR Fine JPGs are quite large in size(>10MB), uploading a large number of them to social media platforms can take a lot of time.
# How to use
Compile it:
```
cargo build --release
```
Put the binary file in `PATH` so you can use it from anywhere.

# Examples
Compress with default parameters:
```
cd your_image_dir
smoljpg
```
The tool comes with the following defaults:
1. Quality: `50`
2. Output directory name: `compressed/`.

Compress with custom parameters:
```
cd your_image_dir/
smoljpg 80 dest/
```
Help:
```
smoljpg -h
```
