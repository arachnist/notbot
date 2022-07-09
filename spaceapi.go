package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"sort"
	"strings"
	"time"

	"gopkg.in/irc.v3"
)

var (
	spaceApiChannels arrayFlags
)

type spaceApiResponse struct {
	API      string `json:"api"`
	Space    string `json:"space"`
	Logo     string `json:"logo"`
	URL      string `json:"url"`
	Location struct {
		Lat     float64 `json:"lat"`
		Lon     float64 `json:"lon"`
		Address string  `json:"address"`
	} `json:"location"`
	State struct {
		Open    bool   `json:"open"`
		Message string `json:"message"`
		Icon    struct {
			Open   string `json:"open"`
			Closed string `json:"closed"`
		} `json:"icon"`
	} `json:"state"`
	Contact struct {
		Facebook string `json:"facebook"`
		Irc      string `json:"irc"`
		Ml       string `json:"ml"`
		Twitter  string `json:"twitter"`
	} `json:"contact"`
	IssueReportChannels []string `json:"issue_report_channels"`
	Projects            []string `json:"projects"`
	Feeds               struct {
		Blog struct {
			Type string `json:"type"`
			URL  string `json:"url"`
		} `json:"blog"`
		Calendar struct {
			Type string `json:"type"`
			URL  string `json:"url"`
		} `json:"calendar"`
		Wiki struct {
			Type string `json:"type"`
			URL  string `json:"url"`
		} `json:"wiki"`
	} `json:"feeds"`
	Sensors struct {
		PeopleNowPresent []struct {
			Value int      `json:"value"`
			Names []string `json:"names"`
		} `json:"people_now_present"`
	} `json:"sensors"`
}

func (s *spaceApiResponse) UserListZWS() (ret []string) {
	for _, room := range s.Sensors.PeopleNowPresent {
		for _, user := range room.Names {
			login := user[:1] + "\u200B" + user[1:]
			ret = append(ret, login)
		}
	}

	return ret
}

type spaceApiClient struct {
	ircChannel string
	apiUrl     string
	users      []string
}

func (s *spaceApiClient) Run(c *irc.Client, done chan bool) {
	ticker := time.NewTicker(10 * time.Second)

	for {
		select {
		case <-done:
			return
		case <-ticker.C:
			var diffText string
			response, err := s.currentState()

			if err != nil {
				log.Println(err)
				break
			}

			current := response.UserListZWS()

			arrived := listSubtract(current, s.users)
			left := listSubtract(s.users, current)
			alsoThere := listSubtract(s.users, left)
			sort.Strings(alsoThere)

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

				msg := fmt.Sprintf("NOTICE %s :%s\n", s.ircChannel, diffText)
				log.Println(diffText)
				c.Write(msg)
				s.users = current
			}
		}
	}
}

func (s *spaceApiClient) currentState() (at spaceApiResponse, err error) {
	var values spaceApiResponse = spaceApiResponse{}

	data, err := httpGet(s.apiUrl)
	if err != nil {
		return values, fmt.Errorf("Unable to access spaceApi:", err)
	}

	err = json.Unmarshal(data, &values)
	if err != nil {
		return values, fmt.Errorf("Unable to decode spaceApi response:", err)
	}

	return values, nil
}

func spaceApiRunWrapper(c *irc.Client, done chan bool) {
	spaceApiDone := make([]chan bool, len(spaceApiChannels))

	for i, ch := range spaceApiChannels {
		args := strings.Split(ch, ",")
		if len(args) != 2 {
			log.Fatalln("Wrong spaceApi channel mapping format", ch, args)
		}

		s := spaceApiClient{
			ircChannel: args[0],
			apiUrl:     args[1],
		}

		go s.Run(c, spaceApiDone[i])
	}

	<-done
	for _, ch := range spaceApiDone {
		ch <- true
	}
}

func init() {
	flag.Var(&spaceApiChannels, "spaceapi.channels", "ircChannel,spaceapi mapping; may be specified multiple times")

	Runners.Add(spaceApiRunWrapper)
}
