CFLAGS ?= -O2 -g -Wall -Wno-unused-parameter
PW := $(shell pkg-config --cflags --libs libpipewire-0.3)

all: s1sink s1play

s1sink: s1sink.c common.h
	$(CC) $(CFLAGS) -o $@ s1sink.c $(PW) -lm

s1play: s1play.c common.h
	$(CC) $(CFLAGS) -o $@ s1play.c $(PW) -lm

clean:
	rm -f s1sink s1play

.PHONY: all clean
