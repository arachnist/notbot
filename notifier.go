package main

import (
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"strconv"
	"time"

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
	x, _ := strconv.Unquote("`\u200B`")
	for _, user := range a.Users {
		login := user.Login[:1] + x + user.Login[1:]
		ret = append(ret, login)
	}

	return ret
}

func listSubstract(a, b []string) (ret []string) {
	mb := make(map[string]bool, len(b))

	for _, x := range b {
		mb[x] = true
	}

	for _, x := range a {
		if _, found := mb[x]; !found {
			ret = append(ret, x)
		}
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

			current := atHS.UserList()

			arrived := listSubstract(current, a.previousUserList)
			left := listSubstract(a.previousUserList, current)

			if len(arrived) > 0 {
				diffText = fmt.Sprint(" +", arrived)
			}

			if len(left) > 0 {
				diffText = fmt.Sprint(" -", left)
			}

			if len(diffText) > 0 {
				msg := fmt.Sprintf("NOTICE #hswaw-bottest :%s\n", diffText)
				log.Println(diffText)
				c.Write(msg)
				a.previousUserList = current
			}
		}
	}
}

func genericHandler(c *irc.Client, m *irc.Message) {
	log.Println(m)
	if m.Command == "001" {
		c.Write("JOIN #hswaw-bottest")
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

func httpGet(link string) ([]byte, error) {
	var buf []byte
	tr := &http.Transport{
		TLSHandshakeTimeout:   20 * time.Second,
		ResponseHeaderTimeout: 20 * time.Second,
	}
	client := &http.Client{
		Transport: tr,
	}

	req, err := http.NewRequest("GET", link, nil)
	if err != nil {
		return []byte{}, err
	}

	resp, err := client.Do(req)
	if err != nil {
		return []byte{}, err
	}
	defer resp.Body.Close()

	// Limit response to 5MiB
	limitedResponse := http.MaxBytesReader(nil, resp.Body, 5*1024*1024)
	buf = make([]byte, 5*1024*1024)

	i, err := io.ReadFull(limitedResponse, buf)
	if err == io.ErrUnexpectedEOF {
		buf = buf[:i]
	} else if err != nil {
		return []byte{}, err
	}

	return buf, nil
}
