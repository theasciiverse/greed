# Makefile for Greed
#
# SPDX-FileCopyrightText: (C) Eric S. Raymond <esr@thyrsus.com>
# SPDX-License-Identifier: BSD-2-Clause

PREFIX      ?= /usr/local
BINDIR      ?= $(PREFIX)/bin
DATADIR     ?= $(PREFIX)/share
MANDIR      ?= $(DATADIR)/man

SFILE=/usr/games/lib/greed.hs

RUSTC ?= rustc
RUSTFLAGS += -C opt-level=2

VERSION=$(shell sed -n <NEWS.adoc '/^[0-9]/s/:.*//p' | head -1)

# Rules

# Note: to suppress the footers with timestamps being generated in HTML,
# we use "-a nofooter".
# To debug asciidoc problems, you may need to run "xmllint --nonet --noout --valid"
# on the intermediate XML that throws an error.

.SUFFIXES: .html .adoc .6

.adoc.6:
	asciidoctor -D. -a nofooter -b manpage $<
.adoc.html:
	asciidoctor -D. -a nofooter -a webfonts! $<

.PHONY: all clean reflow check spellcheck install uninstall dist release refresh

# Build

all: greed greed.6

greed: greed.rs
	SCOREFILE="$(SFILE)" RELEASE="$(VERSION)" $(RUSTC) --edition=2021 $(RUSTFLAGS) -o greed greed.rs

clean:
	rm -f *~ *.o greed greed-*.tar.gz  greed*.rpm *.html
	rm -f greed.6 manpage.links manpage.refs

# Validate

reflow:
	@rustfmt greed.rs

check:
	@tmp=$$(mktemp -d); trap 'rm -rf "$$tmp"' EXIT; SCOREFILE="$(SFILE)" RELEASE="$(VERSION)" $(RUSTC) --edition=2021 $(RUSTFLAGS) --emit=metadata --out-dir "$$tmp" greed.rs

spellcheck:
	@spellcheck greed.adoc

# Install/uninstall

install: greed.6 uninstall
	install -m 0755 -d $(DESTDIR)$(BINDIR)
	install -m 0755 greed $(DESTDIR)$(BINDIR)/
	install -m 0755 -d $(DESTDIR)$(MANDIR)
	install -m 0755 -d $(DESTDIR)$(MANDIR)/man6
	install -m 0644 greed.6 $(DESTDIR)$(MANDIR)/man6/

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/greed
	rm -f $(DESTDIR)$(MANDIR)/man6/greed.6

# Export

SOURCES = README.adoc NEWS.adoc COPYING Makefile greed.rs greed.adoc control greed-logo.png

greed-$(VERSION).tar.gz: $(SOURCES)
	mkdir greed-$(VERSION)
	cp -r $(SOURCES) greed-$(VERSION)
	tar -czf greed-$(VERSION).tar.gz greed-$(VERSION)
	rm -fr greed-$(VERSION)
	ls -l greed-$(VERSION).tar.gz

dist: greed-$(VERSION).tar.gz

release: greed-$(VERSION).tar.gz greed.html
	shipper version=$(VERSION) | sh -e -x

refresh: greed.html
	shipper -N -w version=$(VERSION) | sh -e -x

# end
