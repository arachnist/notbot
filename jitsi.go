package main

import (
	"encoding/xml"
	"flag"
	"fmt"
	"log"
	"strings"
	"time"

	"golang.org/x/net/websocket"
	"gopkg.in/irc.v3"
)

const pingFrame = "<iq type='get'><ping xmlns='urn:xmpp:ping'/>"

var (
	jitsiChannels arrayFlags
)

// we actually only care about .Type, .Nick, and .X.Item.Jid
type JitsiPresence struct {
	XMLName xml.Name `xml:"presence"`
	Text    string   `xml:",chardata"`
	From    string   `xml:"from,attr"`
	To      string   `xml:"to,attr"`
	Type    string   `xml:"type,attr"`
	Xmlns   string   `xml:"xmlns,attr"`
	StatsID string   `xml:"stats-id"`
	Region  struct {
		Text  string `xml:",chardata"`
		Xmlns string `xml:"xmlns,attr"`
		ID    string `xml:"id,attr"`
	} `xml:"region"`
	C struct {
		Text  string `xml:",chardata"`
		Ver   string `xml:"ver,attr"`
		Hash  string `xml:"hash,attr"`
		Xmlns string `xml:"xmlns,attr"`
		Node  string `xml:"node,attr"`
	} `xml:"c"`
	Features struct {
		Text    string `xml:",chardata"`
		Feature struct {
			Text string `xml:",chardata"`
			Var  string `xml:"var,attr"`
		} `xml:"feature"`
	} `xml:"features"`
	JitsiParticipantRegion    string `xml:"jitsi_participant_region"`
	JitsiParticipantCodecType string `xml:"jitsi_participant_codecType"`
	Nick                      struct {
		Text  string `xml:",chardata"`
		Xmlns string `xml:"xmlns,attr"`
	} `xml:"nick"`
	JitsiParticipantE2eeIdKey string `xml:"jitsi_participant_e2ee.idKey"`
	Audiomuted                string `xml:"audiomuted"`
	Videomuted                string `xml:"videomuted"`
	X                         struct {
		Text  string `xml:",chardata"`
		Xmlns string `xml:"xmlns,attr"`
		Photo string `xml:"photo"`
		Item  struct {
			Text        string `xml:",chardata"`
			Affiliation string `xml:"affiliation,attr"`
			Role        string `xml:"role,attr"`
			Jid         string `xml:"jid,attr"`
		} `xml:"item"`
	} `xml:"x"`
}

type JitsiClient struct {
	nick       string
	server     string
	room       string
	ircChannel string
	done       chan bool
	users      map[string]string // map[jid]nick
}

func (j *JitsiClient) KeepAlive(ws *websocket.Conn) {
	ticker := time.NewTicker(5 * time.Second)

	for {
		select {
		case <-ticker.C:
			if _, err := ws.Write([]byte(pingFrame)); err != nil {
				log.Println("JitsiClient", j.server, j.room, "Error while sending ping", err)
				return
			}
		}
	}
}

func (j *JitsiClient) Run(c *irc.Client, done chan bool) {
	var msg = make([]byte, 64*1024)

	origin := "https://" + j.server
	url := "wss://" + j.server + "/xmpp-websocket?room=" + j.room
	protocol := "xmpp"
	var initFrames = []string{
		"<open to=\"" + j.server + "\" version=\"1.0\" xmlns=\"urn:ietf:params:xml:ns:xmpp-framing\"/>",
		"<auth mechanism=\"ANONYMOUS\" xmlns=\"urn:ietf:params:xml:ns:xmpp-sasl\"/>",
		"<open to=\"" + j.server + "\" version=\"1.0\" xmlns=\"urn:ietf:params:xml:ns:xmpp-framing\"/>",
		"<iq id=\"_bind_auth_2\" type=\"set\" xmlns=\"jabber:client\"><bind xmlns=\"urn:ietf:params:xml:ns:xmpp-bind\"/></iq>",
		"<iq id=\"_session_auth_2\" type=\"set\" xmlns=\"jabber:client\"><session xmlns=\"urn:ietf:params:xml:ns:xmpp-session\"/></iq>",
		"<presence to=\"" + j.room + "@conference." + j.server + "/3344bf4a\" xmlns=\"jabber:client\"><x xmlns=\"http://jabber.org/protocol/muc\"/>" +
			"<stats-id>Joy-4gA</stats-id><region id=\"ffmuc-de1\" xmlns=\"http://jitsi.org/jitsi-meet\"/>" +
			"<c hash=\"sha-1\" node=\"https://jitsi.org/jitsi-meet\" ver=\"ZjoRESHG8S3zyis9xCdYpFmbThk=\" xmlns=\"http://jabber.org/protocol/caps\"/>" +
			"<jitsi_participant_region>ffmuc-de1</jitsi_participant_region><videomuted>true</videomuted><audiomuted>true</audiomuted>" +
			"<jitsi_participant_codecType></jitsi_participant_codecType><nick xmlns=\"http://jabber.org/protocol/nick\">" + j.nick + "</nick></presence>",
	}
	
	j.users = make(map[string]string)

	for {
		log.Println("JitsiClient", j.server, j.room, "Initializing")

		ws, err := websocket.Dial(url, protocol, origin)
		if err != nil {
			log.Println("JitsiClient", j.server, j.room, "Error connecting to websocket", url, err)
			goto reconnect
		}

		for n, frame := range initFrames {
			if _, err := ws.Write([]byte(frame)); err != nil {
				log.Println("JitsiClient", j.server, j.room, "Error sending initialization frame", n, err)
				goto reconnect
			}
		}

		log.Println("JitsiClient", j.server, j.room, "Running")
		go j.KeepAlive(ws)
		for {
			select {
			case <-done:
				log.Println("JitsiClient", j.server, j.room, "Shutting down")
				return
			default:
				_, err := ws.Read(msg)
				v := JitsiPresence{}

				if err != nil {
					log.Println("JitsiClient", j.server, j.room, "Error while reading from websocket", err)
					goto reconnect
				}

				err = xml.Unmarshal(msg, &v)
				if err != nil {
					// xml parsing errors will be normal here
					continue
				}

				if v.Nick.Text != "" { // if presence event has Nick present, it *shouldn't* mean that user has left the chat
					if v.X.Item.Jid != "" {
						if knownNick, ok := j.users[v.X.Item.Jid]; ok {
							if knownNick != v.Nick.Text { // user changed nickname, we don't care about that enough
                                log.Println("JitsiClient", j.server, j.room, "User changed nickname:", knownNick, v.Nick.Text)
								j.users[v.X.Item.Jid] = v.Nick.Text
								continue
							}
						} else { // new user
							j.users[v.X.Item.Jid] = v.Nick.Text
							nickZws := v.Nick.Text[:1] + "\u200B" + v.Nick.Text[1:]
							ircMsg := fmt.Sprintf("NOTICE %s :jitsi: +%s\n", j.ircChannel, nickZws)
                            log.Println("JitsiClient", j.server, j.room, "User joined:", j.users[v.X.Item.Jid])
							c.Write(ircMsg)
							continue
						}
					}
				}
				if v.Type == "unavailable" {
					if v.X.Item.Jid != "" {
						if knownNick, ok := j.users[v.X.Item.Jid]; ok {
							delete(j.users, v.X.Item.Jid)
							nickZws := knownNick[:1] + "\u200B" + knownNick[1:]
							ircMsg := fmt.Sprintf("NOTICE %s :jitsi: -%s\n", j.ircChannel, nickZws)
                            log.Println("JitsiClient", j.server, j.room, "User left:", knownNick)
							c.Write(ircMsg)
							continue
						}
					}
				}
			}
		}
	reconnect:
		time.Sleep(1 * time.Second)
		log.Println("JitsiClient", j.server, j.room, "Reconnecting...")
	}
}

func JitsiRunWrapper(c *irc.Client, done chan bool) {
	jitsiDone := make([]chan bool, len(jitsiChannels))

	for i, ch := range jitsiChannels {
		args := strings.Split(ch, ",")
		if len(args) != 3 {
			log.Fatalln("Wrong jitsi channel mapping format", ch, args)
		}

		j := JitsiClient{
            nick: nickname,
            ircChannel: args[0],
            server: args[1],
            room: args[2],
        }

		go j.Run(c, jitsiDone[i])
	}

	<-done
	for _, ch := range jitsiDone {
		ch <- true
	}
}

func init() {
	flag.Var(&jitsiChannels, "jitsi.channels", "ircChannel,jitsiServer,jitsiRoom mapping; may be specified multiple times")

	Runners.Add(JitsiRunWrapper)
}
