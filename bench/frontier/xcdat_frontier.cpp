// XCDAT under the C² benchmark's protocol, for bench/frontier/run.sh: read one
// key a line, sort and deduplicate, build once, then look every key up once in
// one `std::mt19937{2}` shuffle -- a single timed pass with no warm-up, through
// a non-inlined loop that stores each id in a volatile, as C²'s `query_trie`
// does -- and print `build_ms,size_mib,latency_ns` as its `benchmark.cpp` does.
// XCDAT's own `xcdat_benchmark` samples its queries with replacement and
// averages warm passes instead. Type 15, the `HashedDict` table's rival, then
// times three chained passes -- each query's slot waiting on the answer before
// it, as frontier_lex's chained rows do -- and prints their mean and minimum on
// a line of its own.
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

// Each query's slot chained through the answer before it (`& zero`, `zero`
// opaque): no two lookups overlap, so this is the dependent latency rather than
// the throughput.
template <class Trie>
std::uint64_t __attribute__((noinline))
query_trie_chained(const std::vector<std::string> &keys, const Trie &trie,
                   std::size_t zero) {
  std::uint64_t prev = 0, sink = 0;
  for (std::size_t i = 0; i < keys.size(); i++) {
    prev = trie.lookup(keys[i + (prev & zero)]).value_or(UINT64_MAX);
    sink += prev;
  }
  return sink;
}

template <class Trie>
int run(const std::vector<std::string> &keys, double raw_mib, bool chained) {
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
  if (!chained)
    return 0;
  // Right after the cold pass, as frontier_lex times its own chained passes.
  volatile std::size_t opaque_zero = 0;
  const std::size_t zero = opaque_zero;
  std::uint64_t sink = 0;
  double sum = 0, least = 0;
  for (int pass = 0; pass < 3; pass++) {
    start = std::chrono::high_resolution_clock::now();
    sink += query_trie_chained(queries, trie, zero);
    end = std::chrono::high_resolution_clock::now();
    const double ns =
        std::chrono::duration<double, std::nano>(end - start).count() /
        static_cast<double>(queries.size());
    sum += ns;
    least = pass == 0 ? ns : std::min(least, ns);
  }
  // XCDAT's ids are a permutation of [0, n): any other sum is a wrong answer.
  const std::uint64_t n = queries.size();
  if (sink != 3 * (n * (n - 1) / 2)) {
    std::fprintf(stderr, "xcdat answered a wrong id in a chained pass\n");
    return 1;
  }
  std::printf("chained mean %f / min %f ns\n", sum / 3, least);
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
    return run<xcdat::trie_7_type>(keys, raw_mib, false);
  if (type == "8")
    return run<xcdat::trie_8_type>(keys, raw_mib, false);
  if (type == "15")
    return run<xcdat::trie_15_type>(keys, raw_mib, true);
  if (type == "16")
    return run<xcdat::trie_16_type>(keys, raw_mib, false);
  std::fprintf(stderr, "unknown trie type %s: 7, 8, 15 or 16\n", argv[2]);
  return 2;
}
