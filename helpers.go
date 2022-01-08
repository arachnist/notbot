package main

import (
	"io"
	"net/http"
	"strings"
	"time"
)

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

type arrayFlags []string

func (i *arrayFlags) String() string {
	return strings.Join(*i, ",")
}

func (i *arrayFlags) Set(value string) error {
	*i = append(*i, value)
	return nil
}
