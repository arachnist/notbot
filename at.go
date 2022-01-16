package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"time"

	"gopkg.in/irc.v3"
)

type atMonitor struct {
	previousUserList []string
	channel          string
	apiAddress       string
}

type atUsers struct {
	Login      string
	Timestamp  float64
	PrettyTime string `json:"pretty_time"`
}

type atResponse struct {
	Users   []atUsers
	Esps    uint
	Kektops uint
	Vms     uint
	Unknown uint
}

func (a *atResponse) UserList() (ret []string) {
	for _, user := range a.Users {
		ret = append(ret, user.Login)
	}

	return ret
}

func (a *atResponse) UserListZWS() (ret []string) {
	for _, user := range a.Users {
		login := user.Login[:1] + "\u200B" + user.Login[1:]
		ret = append(ret, login)
	}

	return ret
}

func (a *atMonitor) Run(c *irc.Client, done chan bool) {
	ticker := time.NewTicker(10 * time.Second)

	for {
		select {
		case <-done:
			return
		case <-ticker.C:
			var diffText string
			atHS, err := a.at()

			if err != nil {
				log.Println(err)
				break
			}

			current := atHS.UserListZWS()

			arrived := listSubtract(current, a.previousUserList)
			left := listSubtract(a.previousUserList, current)
			alsoThere := listSubtract(a.previousUserList, left)

			if len(arrived) > 0 {
				diffText = fmt.Sprint(" arrived: ", arrived)
			}

			if len(left) > 0 {
				diffText += fmt.Sprint(" left: ", left)
			}

			if len(diffText) > 0 {
				if len(alsoThere) > 0 {
					diffText += fmt.Sprint(" also there: ", alsoThere)
				}

				msg := fmt.Sprintf("NOTICE %s :%s\n", a.channel, diffText)
				log.Println(diffText)
				c.Write(msg)
				a.previousUserList = current
			}
		}
	}
}

func (a *atMonitor) at() (at atResponse, err error) {
	var values atResponse = atResponse{}

	data, err := httpGet(a.apiAddress)
	if err != nil {
		return values, fmt.Errorf("Unable to access checkinator api:", err)
	}

	err = json.Unmarshal(data, &values)
	if err != nil {
		return values, fmt.Errorf("Unable to decode checkinator response:", err)
	}

	return values, nil
}

var (
	atMonitorInstance atMonitor
)

func init() {
	flag.StringVar(&atMonitorInstance.channel, "at.channel", "#hswaw-members", "Channel to send entrance/exit notices")
	flag.StringVar(&atMonitorInstance.apiAddress, "at.api", "https://at.hackerspace.pl/api", "Checkinator API address")

	Runners.Add(atMonitorInstance.Run)
}
