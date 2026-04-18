# Build libJadeRuntime.a — the jade async runtime library.
#
# Usage:
#   make                  — pthread backend (Linux / macOS)
#   make JADEOS=1         — Jade OS kernel backend (requires Jade OS headers)
#   make CC=clang         — use a different compiler
#   make install PREFIX=/usr/local/lib/jade
#
# The library is linked by `jade build` when async functions are present.
# Point the build tool at it with JADE_RT_LIB=/path/to/this/dir.

CC      ?= cc
AR      ?= ar
CFLAGS  ?= -O2 -Wall -Wextra -std=c11 -fPIC
PREFIX  ?= /usr/local/lib/jade

ifdef JADEOS
SRC     = jade_rt_jadeos.c
CFLAGS += -D__JADE_OS__
else
SRC     = jade_rt_pthread.c
LDFLAGS ?= -lpthread
endif

OBJ = jade_rt.o
LIB = libJadeRuntime.a

.PHONY: all clean install

all: $(LIB)

$(OBJ): $(SRC) jade_rt.h
	$(CC) $(CFLAGS) -c -o $@ $(SRC)

$(LIB): $(OBJ)
	$(AR) rcs $@ $<

install: $(LIB)
	install -d $(PREFIX)
	install -m 644 $(LIB)  $(PREFIX)/$(LIB)
	install -m 644 jade_rt.h $(PREFIX)/jade_rt.h

clean:
	rm -f $(OBJ) $(LIB)
