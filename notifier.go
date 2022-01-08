package main

import (
	"flag"
	"log"
	"net"

	"gopkg.in/irc.v3"
)

var (
	channels arrayFlags
	server   string
	nickname string
	password string
	user     string
	name     string
)

func init() {
	flag.StringVar(&server, "server", "irc.libera.chat:6667", "Server to connect to")
	flag.StringVar(&nickname, "nickname", "notbot", "Bot nickname")
	flag.StringVar(&password, "password", "", "Bot nickserv password")
	flag.StringVar(&user, "user", "bot", "Bot user parameter")
	flag.StringVar(&name, "name", "bot notbot", "Bot real name parameter")
	flag.Var(&channels, "channels", "Channel to join; may be specified multiple times")
}

func main() {
	done := make([]chan bool, len(Runners.list))

	flag.Parse()

	conn, err := net.Dial("tcp", server)
	if err != nil {
		log.Fatalln(err)
	}

	config := irc.ClientConfig{
		Nick:    nickname,
		Pass:    password,
		User:    user,
		Name:    name,
		Handler: irc.HandlerFunc(handlerFactory(Dispatchers.list)),
	}

	client := irc.NewClient(conn, config)

	for i, runner := range Runners.list {
		go runner(client, done[i])
	}

	err = client.Run()
	if err != nil {
		for _, ch := range done {
			ch <- true
		}
		log.Fatalln(err)
	}
}
