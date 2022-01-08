package main

import (
	"log"
	"sync"

	"gopkg.in/irc.v3"
)

type dispatchFunc func(*irc.Client, *irc.Message)
type dispatchers struct {
	lock sync.Mutex
	list []dispatchFunc
}

func (d *dispatchers) Add(f dispatchFunc) {
	d.lock.Lock()
	defer d.lock.Unlock()

	d.list = append(d.list, f)
}

type runFunc func(c *irc.Client, done chan bool)
type runners struct {
	lock sync.Mutex
	list []runFunc
}

func (r *runners) Add(f runFunc) {
	r.lock.Lock()
	defer r.lock.Unlock()

	r.list = append(r.list, f)
}

var (
	Dispatchers dispatchers
	Runners     runners
)

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

func joiner(c *irc.Client, m *irc.Message) {
	if m.Command == "001" {
		for _, ch := range channels {
			c.Write("JOIN " + ch)
		}
	}
}

func init() {
	Dispatchers.Add(logger)
	Dispatchers.Add(joiner)
}
