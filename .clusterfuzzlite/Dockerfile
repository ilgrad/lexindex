# OSS-Fuzz's Rust builder: the latest nightly and cargo-fuzz. The tag floats, as the nightly in
# `fuzz.yml` does. ClusterFuzzLite builds this from its own checkout of the pull request, which is
# the context `COPY` reads.
FROM gcr.io/oss-fuzz-base/base-builder-rust:v1
COPY . $SRC/lexindex
WORKDIR $SRC/lexindex
COPY .clusterfuzzlite/build.sh $SRC/
