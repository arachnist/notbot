package main

import (
	"log"
	"net"

	"gopkg.in/irc.v3"
)

type dispatchFunc func(*irc.Client, *irc.Message)

func handlerFactory(dispatchers []dispatchFunc) func(*irc.Client, *irc.Message) {
	return func(c *irc.Client, m *irc.Message) {
		for _, f := range dispatchers {
			go f(c, m.Copy())
		}
	}
}

func logger(_ *irc.Client, m *irc.Message) {
	log.Println(m)
}

func joinerFactory(channels []string) func(*irc.Client, *irc.Message) {
	return func(c *irc.Client, m *irc.Message) {
		if m.Command == "001" {
			for _, ch := range channels {
				c.Write("JOIN " + ch)
			}
		}
	}
}

func main() {
	done := make(chan bool)
	var a atMonitor
	var dispatchers = []dispatchFunc{
		logger,
		joinerFactory([]string{"#hswaw-members"}),
	}

	conn, err := net.Dial("tcp", "irc.libera.chat:6667")
	if err != nil {
		log.Fatalln(err)
	}

	config := irc.ClientConfig{
		Nick:    "notbot",
		Pass:    "***",
		User:    "bot",
		Name:    "notbot",
		Handler: irc.HandlerFunc(handlerFactory(dispatchers)),
	}

	client := irc.NewClient(conn, config)
	go a.Run(client, done)

	err = client.Run()
	if err != nil {
		done <- true
		log.Fatalln(err)
	}
}
