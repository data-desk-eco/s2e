# flare: one c file over libcurl and libopenjp2 (>= 2.5 for windowed decode),
# as a command (flare) and as a duckdb extension (flare.duckdb_extension).
# include/ holds duckdb v2.0.0's C extension headers; the extension targets
# the stable C API v1.2.0, so it loads in any duckdb from 1.2 on.
PKGS = libcurl libopenjp2
CFLAGS += -O2 -Wall -Wextra -Wno-unused-parameter \
	$(shell pkg-config --cflags $(PKGS))
LDLIBS += $(shell pkg-config --libs $(PKGS)) -lpthread -lm
PLATFORM ?= $(shell uname -sm | awk '{ \
	os = $$1 == "Darwin" ? "osx" : tolower($$1); \
	arch = $$2 ~ /^(x86_64|amd64)$$/ ? "amd64" : "arm64"; \
	print os "_" arch }')
VERSION = 0.1.0

all: flare flare.duckdb_extension

flare: flare.c

# a loadable extension is the shared library plus duckdb's metadata footer:
# a wasm-style custom section header, eight 32-byte fields (last first) and
# 256 bytes left for a signature
flare.duckdb_extension: duckdb.c flare.c
	$(CC) $(CFLAGS) -Iinclude -shared -fPIC -o flare.so duckdb.c $(LDLIBS)
	{ cat flare.so; printf '\000\223\004\020duckdb_signature\200\004'; \
	  for f in "" "" "" C_STRUCT $(VERSION) v1.2.0 $(PLATFORM) 4; do \
	    printf '%s' "$$f"; head -c $$((32 - $${#f})) /dev/zero; done; \
	  head -c 256 /dev/zero; } > $@
	rm flare.so

clean:
	rm -f flare flare.duckdb_extension
.PHONY: all clean

# into ~/.duckdb/extensions, so `load flare` works from any directory
install: flare.duckdb_extension
	duckdb -unsigned -c "force install '$(CURDIR)/flare.duckdb_extension'"
.PHONY: install
