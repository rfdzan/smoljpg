# smoljpg
Bulk compress JPGs in a directory, powered by [turbojpeg](https://github.com/honzasp/rust-turbojpeg).
# Why
DSLR Fine JPGs are quite large in size(>10MB), uploading a large number of them to social media platforms can take a lot of time.
# How to use
Compile it:
```
cargo build --release
```
Put the binary file in `PATH` so you can use it from anywhere.

To use it you must give it two arguments in this order:
1. Final **quality** in percentage, 0 - 100%. So, an argument of `smoljpg 60 test/` would produce images which are 60% their original quality.
2. The output directory name. The directory will be created for you.

Example:
```
cd your_image_dir
smoljpg 60 test/
```
# To be improved
1. Make it multithreaded.
