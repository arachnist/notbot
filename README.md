# notbot

notbot is a matrix bot primarily used around the Warsaw Hackerspace matrix spaces and rooms.
A significant portion of the functionality is hackerspace-specific, but there are some
general-purpose modules present as well.

It includes lua runtime for running slightly modified parts of an irc bot: [mun](https://code.hackerspace.pl/ar/notmun)

It also interfaces with a database of a [klacz](https://code.hackerspace.pl/hswaw/klacz) irc
bot instance, which is written in Common Lisp and uses a weird ORM: [hu.dwim.perec](https://hub.darcs.net/hu.dwim/hu.dwim.perec)
