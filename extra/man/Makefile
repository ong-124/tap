# Man page generation
.PHONY: all clean install

SCDOC = scdoc
MANDIR = /usr/local/share/man

all: tap.1 tap.5 tap-bindings.5

tap.1: tap.1.scd
	$(SCDOC) < $< > $@

tap.5: tap.5.scd
	$(SCDOC) < $< > $@

tap-bindings.5: tap-bindings.5.scd
	$(SCDOC) < $< > $@

install: all
	mkdir -p $(DESTDIR)$(MANDIR)/man1 $(DESTDIR)$(MANDIR)/man5
	install -m644 tap.1 $(DESTDIR)$(MANDIR)/man1/tap.1
	install -m644 tap.5 $(DESTDIR)$(MANDIR)/man5/tap.5
	install -m644 tap-bindings.5 $(DESTDIR)$(MANDIR)/man5/tap-bindings.5

clean:
	rm -f tap.1 tap.5 tap-bindings.5