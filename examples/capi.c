/* The C ABI end to end: build, look up both ways, batch, save, reopen, free.
 *
 *     cargo build --release --features capi
 *     cc -std=c11 -Wall -Wextra -Wpedantic -Werror examples/capi.c -Iinclude -Ltarget/release -llexindex -o capi
 *     LD_LIBRARY_PATH=target/release ./capi
 *
 * Exits non-zero on the first answer that is not the one the index promises, so CI runs it as a test.
 */
#include <inttypes.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "lexindex.h"

static int ok(LexindexStatus status, const char *what) {
    if (status != LEXINDEX_STATUS_OK) {
        fprintf(stderr, "%s: status %d: %s\n", what, (int)status, lexindex_last_error());
        return 0;
    }
    return 1;
}

int main(void) {
    if (lexindex_abi_version() != LEXINDEX_ABI_VERSION) {
        fprintf(stderr, "header describes ABI %u, library speaks %u\n", LEXINDEX_ABI_VERSION,
                lexindex_abi_version());
        return 1;
    }
    printf("lexindex %s, ABI %u\n", lexindex_version(), lexindex_abi_version());

    /* Any order, duplicates welcome: the id is the sorted rank. */
    const char *keys[] = {"cherry", "apple", "banana", "apricot", "apple"};
    size_t lens[5];
    for (size_t i = 0; i < 5; i++) lens[i] = strlen(keys[i]);

    LexindexIndex *index = NULL;
    if (!ok(lexindex_index_build(LEXINDEX_KIND_DICT, keys, lens, 5, &index), "build")) return 1;
    printf("%zu keys, kind %d\n", lexindex_index_len(index), (int)lexindex_index_kind(index));
    if (lexindex_index_len(index) != 4 || lexindex_index_kind(index) != LEXINDEX_KIND_DICT) return 1;

    uint64_t id = 0;
    if (!ok(lexindex_index_id(index, "banana", 6, &id), "id")) return 1;
    printf("banana -> %" PRIu64 "\n", id);
    if (id != 2) return 1;

    if (lexindex_index_id(index, "durian", 6, &id) != LEXINDEX_STATUS_NOT_FOUND) return 1;
    printf("durian -> not found\n");

    char buf[16];
    size_t len = 0;
    if (!ok(lexindex_index_key(index, 0, buf, sizeof buf, &len), "key")) return 1;
    printf("0 -> %s (%zu bytes)\n", buf, len);
    if (len != 5 || strcmp(buf, "apple") != 0) return 1;

    /* Ask for the size first when the key may not fit. */
    if (lexindex_index_key(index, 3, NULL, 0, &len) != LEXINDEX_STATUS_BUFFER_TOO_SMALL || len != 6) return 1;

    uint64_t ids[5];
    if (!ok(lexindex_index_ids(index, keys, lens, 5, ids), "ids")) return 1;
    for (size_t i = 0; i < 5; i++) printf("%s -> %" PRIu64 "\n", keys[i], ids[i]);
    if (ids[0] != 3 || ids[1] != 0 || ids[2] != 2 || ids[3] != 1 || ids[4] != 0) return 1;

    bool held = false;
    if (!ok(lexindex_index_contains(index, "apricot", 7, &held), "contains") || !held) return 1;

    const char *path = "capi-example.bdx";
    if (!ok(lexindex_index_save(index, path), "save")) return 1;
    LexindexIndex *reopened = NULL;
    if (!ok(lexindex_index_open(path, &reopened), "open")) return 1;
    remove(path);
    if (!ok(lexindex_index_id(reopened, "cherry", 6, &id), "id after reopen") || id != 3) return 1;
    printf("saved, reopened: cherry -> %" PRIu64 "\n", id);

    lexindex_index_free(reopened);
    lexindex_index_free(index);
    return 0;
}
