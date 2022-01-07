package main

import (
	"log"
	"net"

	"gopkg.in/irc.v3"
)

func genericHandler(c *irc.Client, m *irc.Message) {
	log.Println(m)
	if m.Command == "001" {
		c.Write("JOIN #hswaw-members")
	}
}

func main() {
	done := make(chan bool)
	var a atMonitor

	conn, err := net.Dial("tcp", "irc.libera.chat:6667")
	if err != nil {
		log.Fatalln(err)
	}

	config := irc.ClientConfig{
		Nick:    "notbot",
		Pass:    "***",
		User:    "bot",
		Name:    "notbot",
		Handler: irc.HandlerFunc(genericHandler),
	}

	client := irc.NewClient(conn, config)
	go a.Run(client, done)

	err = client.Run()
	if err != nil {
		done <- true
		log.Fatalln(err)
	}
}
