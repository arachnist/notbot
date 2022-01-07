package main

import (
	"encoding/json"
	"fmt"
	"time"
    "log"

	"gopkg.in/irc.v3"
)

type atMonitor struct {
	previousUserList []string
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
			atHS, err := at()

			if err != nil {
				log.Println(err)
				break
			}

			current := atHS.UserListZWS()

			arrived := listSubstract(current, a.previousUserList)
			left := listSubstract(a.previousUserList, current)

			if len(arrived) > 0 {
				diffText = fmt.Sprint(" +", arrived)
			}

			if len(left) > 0 {
				diffText = fmt.Sprint(" -", left)
			}

			if len(diffText) > 0 {
				msg := fmt.Sprintf("NOTICE #hswaw-members :%s\n", diffText)
				log.Println(diffText)
				c.Write(msg)
				a.previousUserList = current
			}
		}
	}
}

func at() (at atResponse, err error) {
	var values atResponse = atResponse{}

	data, err := httpGet("https://at.hackerspace.pl/api")
	if err != nil {
		return values, fmt.Errorf("Unable to access checkinator api:", err)
	}

	err = json.Unmarshal(data, &values)
	if err != nil {
		return values, fmt.Errorf("Unable to decode checkinator response:", err)
	}

	return values, nil
}
