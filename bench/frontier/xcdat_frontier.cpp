// XCDAT under the C² benchmark's protocol, for bench/frontier/run.sh: read one
// key a line, sort and deduplicate, build once, then look every key up once in
// one `std::mt19937{2}` shuffle -- a single timed pass with no warm-up, through
// a non-inlined loop that stores each id in a volatile, as C²'s `query_trie`
// does -- and print `build_ms,size_mib,latency_ns` as its `benchmark.cpp` does.
// XCDAT's own `xcdat_benchmark` samples its queries with replacement and
// averages warm passes instead.
#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <fstream>
#include <random>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include <xcdat.hpp>

namespace {

template <class Trie>
void __attribute__((noinline)) query_trie(const std::vector<std::string> &keys,
                                          const Trie &trie) {
  for (const auto &key : keys) {
    volatile std::uint64_t id = trie.lookup(key).value_or(UINT64_MAX);
    (void)id;
  }
}

template <class Trie>
int run(const std::vector<std::string> &keys, double raw_mib) {
  auto start = std::chrono::high_resolution_clock::now();
  const Trie trie(keys);
  auto end = std::chrono::high_resolution_clock::now();
  const double build_ms =
      std::chrono::duration<double, std::milli>(end - start).count();
  const double size_mib =
      static_cast<double>(xcdat::memory_in_bytes(trie)) / (1024.0 * 1024.0);
  std::printf("build time: %f ms\n", build_ms);
  std::printf("space cost: %f MiB (%f%% of original size %f MiB)\n", size_mib,
              size_mib / raw_mib * 100, raw_mib);

  std::vector<std::string> queries = keys;
  std::shuffle(queries.begin(), queries.end(), std::mt19937{2});
  start = std::chrono::high_resolution_clock::now();
  query_trie(queries, trie);
  end = std::chrono::high_resolution_clock::now();
  const double latency_ns =
      std::chrono::duration<double, std::nano>(end - start).count() /
      static_cast<double>(queries.size());
  std::printf("avg latency: %f ns\n", latency_ns);
  std::printf("%f,%f,%f\n", build_ms, size_mib, latency_ns);
  return 0;
}

} // namespace

int main(int argc, char **argv) {
  if (argc != 3) {
    std::fprintf(stderr, "usage: xcdat_frontier <keys.txt> <7|8|15|16>\n");
    return 2;
  }
  std::ifstream file(argv[1]);
  if (!file) {
    std::fprintf(stderr, "cannot open %s\n", argv[1]);
    return 1;
  }
  std::vector<std::string> keys;
  double raw_bytes = 0;
  for (std::string key; std::getline(file, key);) {
    raw_bytes += static_cast<double>(key.size());
    keys.push_back(std::move(key));
  }
  std::sort(keys.begin(), keys.end());
  keys.erase(std::unique(keys.begin(), keys.end()), keys.end());
  const double raw_mib = raw_bytes / (1024.0 * 1024.0);

  const std::string_view type = argv[2];
  if (type == "7")
    return run<xcdat::trie_7_type>(keys, raw_mib);
  if (type == "8")
    return run<xcdat::trie_8_type>(keys, raw_mib);
  if (type == "15")
    return run<xcdat::trie_15_type>(keys, raw_mib);
  if (type == "16")
    return run<xcdat::trie_16_type>(keys, raw_mib);
  std::fprintf(stderr, "unknown trie type %s: 7, 8, 15 or 16\n", argv[2]);
  return 2;
}
